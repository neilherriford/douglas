use crate::Response;
use crate::container::{Container, Repository as ContainerRepository};
use crate::{DockerError, Label, SimpleDockerClient, deserialize_labels, serialize_labels};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::Value as Json;
use simple_rest_client::{Header, Request};

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

#[derive(Debug, Serialize, PartialEq)]
struct CreationBody {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Labels")]
    #[serde(serialize_with = "serialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreationResponse {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct ConnectionBody {
    #[serde(rename = "Container")]
    container_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConnectionError {
    message: String,
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
    async fn create(&mut self, name: &str, lables: Vec<Label>) -> Result<Network, DockerError>;
    async fn connect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError>;
    async fn disconnect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn inspect_by_id(&mut self, id: String) -> Result<Network, DockerError> {
        self.expect_non_empty_string_argument("id", &id)?;
        self.inspect_network_by_hight::<Network>(id).await
    }
    async fn inspect_by_name(&mut self, name: String) -> Result<Network, DockerError> {
        self.expect_non_empty_string_argument("name", &name)?;
        self.inspect_network_by_hight::<Network>(name).await
    }

    async fn find_connected_containers_by_id(
        &mut self,
        network_id: String,
    ) -> Result<Vec<Container>, DockerError> {
        self.expect_non_empty_string_argument("network_id", &network_id)?;
        self.find_connected_containers_by_hight(network_id).await
    }

    async fn find_connected_containers_by_name(
        &mut self,
        network_name: String,
    ) -> Result<Vec<Container>, DockerError> {
        self.expect_non_empty_string_argument("network_name", &network_name)?;
        self.find_connected_containers_by_hight(network_name).await
    }

    async fn create(&mut self, name: &str, labels: Vec<Label>) -> Result<Network, DockerError> {
        let req = Request::Post {
            path: "/networks/create".to_string(),
            body: Some(serde_json::to_string(&CreationBody {
                name: name.to_string(),
                labels,
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;
        let body = self.expect_created(response)?;
        let buffer = self.expect_single_chunk::<CreationResponse>(body)?;

        self.inspect_network_by_hight(buffer.id).await
    }

    async fn connect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        let req = Request::Post {
            path: format!("/networks/{}/connect", network.id),
            body: Some(serde_json::to_string(&ConnectionBody {
                container_id: container.id.to_string(),
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;
        self.expect_okay(response)?;
        Ok(())
    }

    async fn disconnect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        let req = Request::Post {
            path: format!("/networks/{}/disconnect", network.id),
            body: Some(serde_json::to_string(&ConnectionBody {
                container_id: container.id.to_string(),
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;

        if !self.is_already_disconnected(&response) {
            self.expect_okay(response)?;
        }
        Ok(())
    }
}

impl SimpleDockerClient {
    async fn inspect_network_by_hight<T>(&mut self, hight: String) -> Result<T, DockerError>
    where
        T: DeserializeOwned,
    {
        let request = Request::Get {
            path: format!("/networks/{}", hight),
            headers: vec![],
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&request).await?;
        let body = self.expect_okay(response)?;
        Ok(self.expect_single_chunk::<T>(body)?)
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

    fn is_already_disconnected(&mut self, response: &Response<Vec<Json>>) -> bool {
        if let Response::Error {
            status: 500,
            body: Some(chunks),
            ..
        } = response
        {
            if let Ok(connection_error) =
                self.expect_single_chunk::<ConnectionError>(chunks.to_vec())
            {
                if connection_error.message.contains("not connected") {
                    return true;
                }
            }
        }

        false
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

    mod inspect_by_id {
        use super::super::*;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, String::new()).await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "id" && given == String::new()
            ));
        }
    }

    mod find_connected_containers_by_id {
        use super::super::*;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result =
                Repository::find_connected_containers_by_id(&mut client, String::new()).await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "network_id" && given == String::new()
            ));
        }
    }

    mod find_connected_containers_by_name {
        use super::super::*;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result =
                Repository::find_connected_containers_by_name(&mut client, String::new()).await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "network_name" && given == String::new()
            ));
        }
    }

    mod inspect_by_name {
        use super::super::*;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_name(&mut client, String::new()).await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "name" && given == String::new()
            ));
        }
    }

    mod inspect_network_by_hight {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

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

            assert!(matches!(result, Err(DockerError::InsufficientChunksError)));
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

            assert!(matches!(result, Err(DockerError::ExcessiveChunksError)));
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
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find_connected_containers_by_hight("10111213".to_string())
                .await;

            assert!(matches!(result, Err(DockerError::InsufficientChunksError)));
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

            assert!(matches!(result, Err(DockerError::ExcessiveChunksError)));
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

    mod create {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_okay() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/networks/create",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&CreationBody {
                        name: "foo".to_string(),
                        labels: vec![Label::new("bar", "baz")],
                    })
                    .unwrap(),
                ),
                None,
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.create("foo", vec![Label::new("bar", "baz")]).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 200, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_created_with_insufficient_chunks() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created(
                "/networks/create",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&CreationBody {
                        name: "foo".to_string(),
                        labels: vec![Label::new("bar", "baz")],
                    })
                    .unwrap(),
                ),
                Some(vec![]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.create("foo", vec![Label::new("bar", "baz")]).await;

            assert!(matches!(result, Err(DockerError::InsufficientChunksError)));
        }

        #[tokio::test]
        async fn should_create_network() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created(
                "/networks/create",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&CreationBody {
                        name: "foo".to_string(),
                        labels: vec![Label::new("bar", "baz")],
                    })
                    .unwrap(),
                ),
                Some(vec![json!({"Id":"123456","Warning":""})]),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/networks/123456",
                Some(vec![json!({
                    "Id": "123456",
                    "Name": "qux",
                    "Labels": {
                      "quux": "corge"
                    }
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.create("foo", vec![Label::new("bar", "baz")]).await;
            let expected = Network {
                id: "123456".to_string(),
                name: "qux".to_string(),
                labels: vec![Label {
                    name: "quux".to_string(),
                    value: "corge".to_string(),
                }],
            };

            assert!(matches!(result, Ok(actual) if actual == expected));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_no_content(
                "/networks/create",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&CreationBody {
                        name: "foo".to_string(),
                        labels: vec![Label::new("bar", "baz")],
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };
            let result = client.create("foo", vec![Label::new("bar", "baz")]).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_internal_server_error(
                "/networks/create",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&CreationBody {
                        name: "foo".to_string(),
                        labels: vec![Label::new("bar", "baz")],
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };
            let result = client.create("foo", vec![Label::new("bar", "baz")]).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }
    }

    mod connect {
        use super::super::*;
        use crate::{Id, container::Status, image::Image};
        use simple_rest_client::MockRestClient;
        use std::collections::HashSet;

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created(
                "/networks/123456/connect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
                Some(vec![]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.connect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_no_content(
                "/networks/123456/connect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.connect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_err() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_internal_server_error(
                "/networks/123456/connect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.connect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_not_found(
                "/networks/123456/connect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.connect(&network, &container).await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_connect() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/networks/123456/connect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
                Some(vec![]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.connect(&network, &container).await;
            assert!(matches!(result, Ok(())));
        }
    }

    mod disconnect {
        use super::super::*;
        use crate::{Id, container::Status, image::Image};
        use serde_json::json;
        use simple_rest_client::MockRestClient;
        use std::collections::HashSet;

        #[tokio::test]
        async fn should_return_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_no_content(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_return_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
                None,
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_return_err_if_err() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_internal_server_error(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_return_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_not_found(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;
            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_disconnect_container() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
                Some(vec![]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;
            assert!(matches!(result, Ok(())));
        }

        #[tokio::test]
        async fn should_disconnect_container_even_if_already_disconnected() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return(
                "/networks/123456/disconnect",
                vec![Header::content_type_json()],
                Some(
                    serde_json::to_string(&ConnectionBody {
                        container_id: "654321".to_string(),
                    })
                    .unwrap(),
                ),
                500,
                Some(vec![
                    json!({"message":"container 654321 is not connected to the network foo"}),
                ]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let network = Network {
                id: "123456".to_string(),
                name: "foo".to_string(),
                labels: vec![],
            };

            let container = Container {
                id: "654321".to_string(),
                name: "bar".to_string(),
                image: Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "101112".to_string(),
                    },
                    tags: HashSet::new(),
                },
                status: Status::Dead,
                mounts: vec![],
                environment_variables: vec![],
                labels: vec![],
            };

            let result = client.disconnect(&network, &container).await;
            assert!(matches!(result, Ok(())));
        }
    }
}
