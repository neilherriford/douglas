use crate::container::{Container, Repository as ContainerRepository};
use crate::{DockerError, Label, SimpleDockerClient, deserialize_labels};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::value::Value as Json;
use simple_rest_client::Request;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Network {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConnectedContainers {
    #[serde(rename = "Containers")]
    #[serde(deserialize_with = "deserialize_keys")]
    pub container_ids: Vec<String>,
}

pub(crate) fn deserialize_keys<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Containers to be an object"))?;

    Ok(obj.keys().map(|key| key.as_str().to_string()).collect())
}

#[async_trait::async_trait]
pub trait Repository {
    async fn inspect_by_id(&mut self, id: String) -> Result<Network, DockerError>;
    async fn inspect_by_name(&mut self, name: String) -> Result<Network, DockerError>;
    async fn find_connected_containers_by_id(
        &mut self,
        network_id: String,
    ) -> Result<Vec<Container>, DockerError>;
    async fn find_connected_containers_by_name(
        &mut self,
        network_name: String,
    ) -> Result<Vec<Container>, DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn inspect_by_id(&mut self, id: String) -> Result<Network, DockerError> {
        self.inspect_network_by_hight::<Network>(id).await
    }
    async fn inspect_by_name(&mut self, name: String) -> Result<Network, DockerError> {
        self.inspect_network_by_hight::<Network>(name).await
    }

    async fn find_connected_containers_by_id(
        &mut self,
        network_id: String,
    ) -> Result<Vec<Container>, DockerError> {
        self.find_connected_containers_by_hight(network_id).await
    }

    async fn find_connected_containers_by_name(
        &mut self,
        network_name: String,
    ) -> Result<Vec<Container>, DockerError> {
        self.find_connected_containers_by_hight(network_name).await
    }
}

impl SimpleDockerClient {
    async fn inspect_network_by_hight<T>(&mut self, hight: String) -> Result<T, DockerError>
    where
        T: DeserializeOwned,
    {
        let request = Request::Get {
            path: format!("/networks/{}", hight),
            headers: None,
        };

        Ok(self.expect_single_chunk::<T>(request).await?)
    }

    async fn find_connected_containers_by_hight(
        &mut self,
        hight: String,
    ) -> Result<Vec<Container>, DockerError> {
        let network = self
            .inspect_network_by_hight::<ConnectedContainers>(hight)
            .await?;
        let mut result: Vec<Container> = Vec::with_capacity(network.container_ids.len());

        for container_id in network.container_ids {
            result.push(ContainerRepository::inspect_by_id(self, container_id).await?);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    mod key_deserializer {
        use super::super::*;
        use crate::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_keys")]
            pub keys: Vec<String>,
        }

        #[test]
        fn should_err_if_not_an_obj() {
            let json = r#"
                {
                  "Wrapper":
                  {
                    "keys": []
                  }
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_collect_keys() {
            let json = r#"
                {
                  "keys": {
                    "foo": true,
                    "bar": 123
                  }
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);

            assert!(
                matches!(result, Ok(actual) if actual.keys.clone().sort() == vec!["foo", "bar"].sort())
            )
        }
    }

    mod inspect_network_by_hight {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_missing_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    message: msg,
                    ..
                }) if msg == "no results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_too_many_chunks() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(vec![json!("too"), json!("many")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    message: msg,
                    ..
                }) if msg == "too many results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_bad_json() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_okay("/networks/10111213", Some(vec![json!("oops")]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_network_by_hight::<Network>("10111213".to_string())
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_inspect_by_id() {
            let mut mock_rest_client = MockRestClient::new();
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

            let result = client
                .inspect_network_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Ok(Network {
                    id,
                    name,
                    labels
                }) if id == "10111213" && name == "qux" && labels == vec![Label {
                    name: "quux".to_string(),
                    value: "corge".to_string()
                }]
            ));
        }
    }

    mod find_connected_containers_by_hight {
        use super::super::*;
        use crate::Id;
        use crate::container::{EnvironmentVariable, Mount, MountType, Status};
        use crate::image::{Image, Tag};
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_missing_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;
            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 200, message, ..}) if message == "no results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_too_many_chunks() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(vec![json!("too"), json!("many")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 200, message, ..}) if message == "too many results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/networks/10111213");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_find_connected_containers_by_hight() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(vec![json!({
                    "Containers": {
                        "123456": {}
                    }
                })]),
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

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

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
