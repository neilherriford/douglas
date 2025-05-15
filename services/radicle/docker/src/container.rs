use crate::image::{Image, Repository as ImageRepository};
use crate::{DockerError, Id, Request, SimpleDockerClient, deserialize_id};
use serde::{Deserialize, Deserializer};
use serde_json::value::Value as Json;

#[derive(Debug, Deserialize, PartialEq)]
struct ContainerBuffer {
    #[serde(rename = "Id")]
    pub id: String,

    #[serde(rename = "Name")]
    pub name: String,

    #[serde(rename = "State")]
    pub state: State,

    #[serde(rename = "Image")]
    #[serde(deserialize_with = "deserialize_id")]
    pub image_id: Id,

    #[serde(rename = "Mounts")]
    pub mounts: Vec<Mount>,

    #[serde(deserialize_with = "deserialize_networks")]
    #[serde(rename = "NetworkSettings")]
    pub networks: Vec<String>,

    #[serde(rename = "Config")]
    pub config: Config,
}

#[derive(Debug, Deserialize, PartialEq)]
struct State {
    #[serde(rename = "Status")]
    pub status: Status,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Config {
    #[serde(rename = "Env")]
    #[serde(deserialize_with = "deserialize_environment_variables")]
    pub env: Vec<EnvironmentVariable>,

    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, PartialEq)]
pub struct Label {
    pub name: String,
    pub value: String,
}

#[derive(Debug, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: Image,
    pub status: Status,
    pub mounts: Vec<Mount>,
    pub networks: Vec<String>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Mount {
    #[serde(rename = "Type")]
    pub mount_type: MountType,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Destination")]
    pub destination: String,
    #[serde(rename = "RW")]
    pub writable: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    Bind,
    Volume,
    Image,
    Tmpfs,
    Npipe,
    Cluster,
}

#[derive(Debug, PartialEq, Ord, PartialOrd, Eq)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

fn deserialize_networks<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let networks = json
        .get("Networks")
        .ok_or_else(|| serde::de::Error::missing_field("Networks"))?;

    let obj = networks
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Networks to be an object"))?;

    Ok(obj.keys().cloned().collect())
}

fn deserialize_environment_variables<'de, D>(
    deserializer: D,
) -> Result<Vec<EnvironmentVariable>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_array()
        .ok_or_else(|| serde::de::Error::custom("Expected Env to be an array"))?;

    let result: Vec<EnvironmentVariable> = obj
        .iter()
        .map(|item| {
            if let Some(assignment) = item.as_str() {
                let parts: Vec<&str> = assignment.split('=').collect();
                let (name, value) = match parts.as_slice() {
                    [first, second] => (first.to_string(), second.to_string()),
                    _ => (assignment.to_string(), String::new()),
                };
                Ok(EnvironmentVariable { name, value })
            } else {
                Err(serde::de::Error::custom(
                    "Expected assignments to be strings",
                ))
            }
        })
        .collect::<Result<_, D::Error>>()?;
    Ok(result)
}

fn deserialize_labels<'de, D>(deserializer: D) -> Result<Vec<Label>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Lables to be an object"))?;

    let result = obj
        .iter()
        .map(|(name, value)| {
            if let Some(value) = value.as_str() {
                Ok(Label {
                    name: name.as_str().to_string(),
                    value: value.to_string(),
                })
            } else {
                Err(serde::de::Error::custom("Expected value to be a string"))
            }
        })
        .collect::<Result<_, D::Error>>()?;

    Ok(result)
}

#[async_trait::async_trait]
pub trait Repository {
    async fn inspect_by_id(&mut self, id: String) -> Result<Container, DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn inspect_by_id(&mut self, id: String) -> Result<Container, DockerError> {
        let request = Request::Get {
            path: format!("/containers/{}/json", id),
            headers: None,
        };

        let buffer = self.inspect::<ContainerBuffer>(request).await?;
        let image = ImageRepository::inspect_by_id(self, buffer.image_id).await?;

        let result = Container {
            id: buffer.id,
            name: buffer.name,
            image,
            status: buffer.state.status,
            mounts: buffer.mounts,
            networks: buffer.networks,
            environment_variables: buffer.config.env,
            labels: buffer.config.labels,
        };

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    mod networks_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_networks")]
            #[serde(rename = "Wrapper")]
            networks: Vec<String>,
        }

        #[test]
        fn should_err_if_missing_networks() {
            let json = r#"
                {
                  "Wrapper":
                  {
                    "Oops": {}
                  }
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_err_if_networks_is_not_an_object() {
            let json = r#"
                {
                  "Wrapper":
                  {
                    "Networks": true
                  }
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_deserialize_netowrks() {
            let json = r#"
                {
                  "Wrapper":
                  {
                    "Networks": {
                        "foo": {},
                        "bar": {}
                    }
                  }
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            let mut actual = wrapper.networks;
            actual.sort();
            assert_eq!(vec!["bar".to_string(), "foo".to_string()], actual);
        }
    }

    mod environment_variables_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_environment_variables")]
            environment_variables: Vec<EnvironmentVariable>,
        }

        #[test]
        fn should_err_if_unexpected_format() {
            let json = r#"
                {
                    "environment_variables": false
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_err_if_unexpected_data_type() {
            let json = r#"
                {
                    "environment_variables": [false]
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_deserialize_environment_variables() {
            let json = r#"
                {
                    "environment_variables": [
                        "FOO=bar",
                        "BAZ=qux"
                    ]
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            let mut actual = wrapper.environment_variables;
            actual.sort();

            assert_eq!(
                vec![
                    EnvironmentVariable {
                        name: "BAZ".to_string(),
                        value: "qux".to_string()
                    },
                    EnvironmentVariable {
                        name: "FOO".to_string(),
                        value: "bar".to_string()
                    },
                ],
                actual
            );
        }

        #[test]
        fn should_use_name_if_unexpected_format() {
            let json = r#"
                {
                    "environment_variables": [
                        "FOO"
                    ]
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            let actual = wrapper.environment_variables;

            assert_eq!(
                vec![EnvironmentVariable {
                    name: "FOO".to_string(),
                    value: String::new(),
                },],
                actual
            );
        }
    }

    mod labels_deserializers {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_err_if_invalid_format() {
            let json = r#"
                {
                    "labels": 4
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_err_if_invalid_data() {
            let json = r#"
                {
                    "labels": {"magic": 42}
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_deserialize_labels() {
            let json = r#"
                {
                    "labels": {
                        "foo": "bar",
                        "baz": "qux"
                    }
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();

            assert_eq!(
                vec![
                    Label {
                        name: "baz".to_string(),
                        value: "qux".to_string()
                    },
                    Label {
                        name: "foo".to_string(),
                        value: "bar".to_string()
                    },
                ],
                wrapper.labels
            );
        }
    }

    mod inspect {
        use super::super::*;
        use crate::container::Repository;
        use crate::image::Tag;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;
        use simple_rest_client::Response;

        #[tokio::test]
        async fn should_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)))
        }

        #[tokio::test]
        async fn should_error_if_body_empty() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    body: None,
                    message,
                }) => assert!(message.contains("no result")),
                _ => unreachable!("Expected error!"),
            }
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Created {
                        headers: vec![],
                        body: Some(vec![]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 201,
                    body: _,
                    message,
                }) => assert!(message.contains("expected OK, but recieved CREATED")),
                _ => unreachable!("Expected error!"),
            }
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| Ok(Response::<Vec<Json>>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 204,
                    body: _,
                    message,
                }) => assert!(message.contains("expected OK, but recieved NO CONTENT")),
                _ => unreachable!("Expected error!"),
            }
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Error {
                        status: 500,
                        headers: vec![],
                        body: Some(vec![]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 500,
                    body: _,
                    message: _,
                })
            ));
        }

        #[tokio::test]
        async fn should_error_if() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/containers/123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!(
                            {
                              "Id": "123456",
                              "Name": "foo",
                              "Image": "alg:654321",
                              "State": {"Status": "exited"},
                              "Mounts":
                              [
                                {
                                  "Type": "bind",
                                  "Source": "/bar/",
                                  "Destination": "/baz/",
                                  "RW": true
                                }
                              ],
                              "NetworkSettings":
                              {
                                "Networks": { "qux": {} }
                              },
                              "Config":
                              {
                                "Env": ["quux=corge"],
                                "Labels": {"grault": "garply"}
                              }
                            }
                        )]),
                    })
                });

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/alg:654321/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:654321",
                          "RepoTags":["waldo:1.2.3"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            match result {
                Ok(actual) => {
                    let expected = Container {
                        id: "123456".to_string(),
                        name: "foo".to_string(),
                        image: Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "654321".to_string(),
                            },
                            tags: vec![Tag {
                                name: "waldo".to_string(),
                                version: "1.2.3".to_string(),
                            }],
                        },
                        status: Status::Exited,
                        mounts: vec![Mount {
                            mount_type: MountType::Bind,
                            source: "/bar/".to_string(),
                            destination: "/baz/".to_string(),
                            writable: true,
                        }],
                        networks: vec!["qux".to_string()],
                        environment_variables: vec![EnvironmentVariable {
                            name: "quux".to_string(),
                            value: "corge".to_string(),
                        }],
                        labels: vec![Label {
                            name: "grault".to_string(),
                            value: "garply".to_string(),
                        }],
                    };

                    assert_eq!(expected, actual);
                }
                _ => unreachable!("Unexpeted result"),
            }
        }
    }
}
