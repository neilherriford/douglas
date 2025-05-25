use crate::image::{Image, Repository as ImageRepository};
use crate::network::{Network, Repository as NetworkRepository};
use crate::{
    DockerError, Id, Label, Request, SimpleDockerClient, deserialize_id, deserialize_labels,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::Value as Json;
use simple_rest_client::create_path_and_query_string;
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Serialize, PartialEq)]
struct Filter {
    pub name: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct InspectedContainer {
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
    pub network_ids: Vec<String>,

    #[serde(rename = "Config")]
    pub config: Config,
}

#[derive(Debug, Deserialize, PartialEq)]
struct IdentifiedContainer {
    #[serde(rename = "Id")]
    pub id: String,
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
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: Image,
    pub status: Status,
    pub mounts: Vec<Mount>,
    pub networks: Vec<Network>,
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

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            Status::Created => "created",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Restarting => "restarting",
            Status::Removing => "removing",
            Status::Exited => "exited",
            Status::Dead => "dead",
        };
        write!(f, "{}", text)
    }
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

impl fmt::Display for MountType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = match self {
            MountType::Bind => "bind",
            MountType::Volume => "volume",
            MountType::Image => "image",
            MountType::Tmpfs => "tmpfs",
            MountType::Npipe => "npipe",
            MountType::Cluster => "cluster",
        };
        write!(f, "{}", text)
    }
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

    let network_ids = obj
        .values()
        .map(|def| {
            let summary = def.as_object().ok_or_else(|| {
                serde::de::Error::custom("Expected Network defintion to be an object")
            })?;

            Ok(summary
                .get("NetworkID")
                .ok_or_else(|| serde::de::Error::missing_field("NetworkID"))?
                .as_str()
                .ok_or_else(|| serde::de::Error::custom("Expected NetworkID to be a string"))?
                .to_string())
        })
        .collect::<Result<_, D::Error>>()?;

    Ok(network_ids)
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

#[async_trait::async_trait]
pub trait Repository {
    async fn inspect_by_id(&mut self, id: String) -> Result<Container, DockerError>;
    async fn find_by_name(&mut self, name: String) -> Result<Vec<Container>, DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn inspect_by_id(&mut self, id: String) -> Result<Container, DockerError> {
        let request = Request::Get {
            path: format!("/containers/{}/json", id),
            headers: None,
        };

        let response = self.rest_client.execute(&request).await?;
        let body = self.expect_okay_with_body(response)?;
        let buffer = self.expect_single_chunk::<InspectedContainer>(body)?;

        let image = ImageRepository::inspect_by_id(self, &buffer.image_id).await?;
        let networks = self.inspect_networks(buffer.network_ids).await?;

        let result = Container {
            id: buffer.id,
            name: buffer.name,
            image,
            status: buffer.state.status,
            mounts: buffer.mounts,
            networks,
            environment_variables: buffer.config.env,
            labels: buffer.config.labels,
        };

        Ok(result)
    }

    async fn find_by_name(&mut self, name: String) -> Result<Vec<Container>, DockerError> {
        let filter = serde_json::to_string(&Filter { name: vec![name] })?;
        let request = Request::Get {
            path: create_path_and_query_string(
                "/containers/json",
                HashMap::from([("all", "true"), ("filters", &filter)]),
            ),
            headers: None,
        };

        let response = self.rest_client.execute(&request).await?;
        let body = self.expect_okay_with_body(response)?;
        let buffers = self.expect_single_chunk::<Vec<IdentifiedContainer>>(body)?;

        let mut result: Vec<Container> = Vec::with_capacity(buffers.len());

        for buffer in buffers {
            let container = Repository::inspect_by_id(self, buffer.id).await?;
            result.push(container);
        }

        Ok(result)
    }
}

impl SimpleDockerClient {
    async fn inspect_networks(
        &mut self,
        network_ids: Vec<String>,
    ) -> Result<Vec<Network>, DockerError> {
        let mut result: Vec<Network> = Vec::with_capacity(network_ids.len());
        for network_id in network_ids {
            result.push(NetworkRepository::inspect_by_id(self, network_id).await?);
        }

        return Ok(result);
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
                        "foo": {"NetworkID": "0"},
                        "bar": {"NetworkID": "1"}
                    }
                  }
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            let mut actual = wrapper.networks;
            actual.sort();
            assert_eq!(vec!["0".to_string(), "1".to_string()], actual);
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

    mod inspect_by_id {
        use super::super::*;
        use crate::container::Repository;
        use crate::image::Tag;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/containers/123456/json", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)))
        }

        #[tokio::test]
        async fn should_error_if_body_empty() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/containers/123456/json", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(result, Err(DockerError::InsufficientChunksError)));
        }

        #[tokio::test]
        async fn should_error_if_body_has_multiple_chunks() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/123456/json",
                Some(vec![json!("too"), json!("many")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(result, Err(DockerError::ExcessiveChunksError)));
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/containers/123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/containers/123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/containers/123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/containers/123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_inspect() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/123456/json",
                Some(vec![json!(
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
                        "Networks": {
                            "qux": {
                                "NetworkID": "10111213"
                            }
                        }
                      },
                      "Config":
                      {
                        "Env": ["quux=corge"],
                        "Labels": {"grault": "garply"}
                      }
                    }
                )]),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:654321/json",
                Some(vec![json!({
                "Id": "alg:654321",
                  "RepoTags":["waldo:1.2.3"],
                })]),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(vec![json!({
                    "Id": "10111213",
                    "Name": "qux",
                    "Labels": {
                      "quux": "corge"
                    }
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "123456".to_string()).await;
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
                    }]
                    .into_iter()
                    .collect(),
                },
                status: Status::Exited,
                mounts: vec![Mount {
                    mount_type: MountType::Bind,
                    source: "/bar/".to_string(),
                    destination: "/baz/".to_string(),
                    writable: true,
                }],
                networks: vec![Network {
                    id: "10111213".to_string(),
                    name: "qux".to_string(),
                    labels: vec![Label {
                        name: "quux".to_string(),
                        value: "corge".to_string(),
                    }],
                }],
                environment_variables: vec![EnvironmentVariable {
                    name: "quux".to_string(),
                    value: "corge".to_string(),
                }],
                labels: vec![Label {
                    name: "grault".to_string(),
                    value: "garply".to_string(),
                }],
            };

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }

    mod find_by_name {
        use super::super::*;
        use crate::container::Repository;
        use crate::image::Tag;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_no_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
                None,
            );
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;
            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
                Some(vec![]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;

            assert!(matches!(result, Err(DockerError::InsufficientChunksError)));
        }

        #[tokio::test]
        async fn should_err_if_too_many_chunks() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
                Some(vec![json!("too"), json!("many")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;

            assert!(matches!(result, Err(DockerError::ExcessiveChunksError)));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_find_by_name() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client.expect_get_and_return_okay(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
                Some(vec![json!([{ "Id": "123456" }])]),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/containers/123456/json",
                Some(vec![json!(
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
                        "Networks": {
                            "qux": {
                                "NetworkID": "10111213"
                            }
                        }
                      },
                      "Config":
                      {
                        "Env": ["quux=corge"],
                        "Labels": {"grault": "garply"}
                      }
                    }
                )]),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:654321/json",
                Some(vec![json!(
                    {
                      "Id": "alg:654321",
                      "RepoTags":["waldo:1.2.3"],
                    }
                )]),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(vec![json!(
                    {
                        "Id": "10111213",
                        "Name": "qux",
                        "Labels": {
                          "quux": "corge"
                        }
                    }
                )]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::find_by_name(&mut client, "foo".to_string()).await;
            let expected = vec![Container {
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
                    }]
                    .into_iter()
                    .collect(),
                },
                status: Status::Exited,
                mounts: vec![Mount {
                    mount_type: MountType::Bind,
                    source: "/bar/".to_string(),
                    destination: "/baz/".to_string(),
                    writable: true,
                }],
                networks: vec![Network {
                    id: "10111213".to_string(),
                    name: "qux".to_string(),
                    labels: vec![Label {
                        name: "quux".to_string(),
                        value: "corge".to_string(),
                    }],
                }],
                environment_variables: vec![EnvironmentVariable {
                    name: "quux".to_string(),
                    value: "corge".to_string(),
                }],
                labels: vec![Label {
                    name: "grault".to_string(),
                    value: "garply".to_string(),
                }],
            }];

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }
}
