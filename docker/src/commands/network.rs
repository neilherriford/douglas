use super::{
    assert_created_with_body, assert_non_empty_string_argument, assert_okay, assert_okay_with_body,
};
use crate::{Container, DockerError, Label, deserialize_labels, serialize_labels};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Header, Request, Response, RestClient};
use std::sync::Arc;

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

pub struct NetworkCommand {
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
}

impl NetworkCommand {
    pub fn new(
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            rest_client,
            parser,
        }
    }

    pub async fn inspect_by_id(&mut self, id: &str) -> Result<Network, DockerError> {
        assert_non_empty_string_argument("id", id)?;
        self.inspect_network_by_hight::<Network>(id).await
    }

    pub async fn inspect_by_name(&mut self, name: &str) -> Result<Network, DockerError> {
        assert_non_empty_string_argument("name", name)?;
        self.inspect_network_by_hight::<Network>(name).await
    }

    pub async fn find_connected_containers_by_id(
        &mut self,
        network_id: &str,
    ) -> Result<Vec<String>, DockerError> {
        assert_non_empty_string_argument("network_id", network_id)?;
        self.find_connected_containers_by_hight(network_id).await
    }

    pub async fn find_connected_containers_by_name(
        &mut self,
        network_name: &str,
    ) -> Result<Vec<String>, DockerError> {
        assert_non_empty_string_argument("network_name", network_name)?;
        self.find_connected_containers_by_hight(network_name).await
    }

    pub async fn create(&mut self, name: &str, labels: Vec<Label>) -> Result<Network, DockerError> {
        let req = Request::Post {
            path: "/networks/create".to_string(),
            body: Some(serde_json::to_string(&CreationBody {
                name: name.to_string(),
                labels,
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&req).await?
        };

        let body = assert_created_with_body(response)?;
        let json = self.parser.parse(body)?;
        let result: CreationResponse = from_value(json)?;

        self.inspect_network_by_hight(&result.id).await
    }

    pub async fn connect(
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

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&req).await?;
        assert_okay(response)?;
        Ok(())
    }

    pub async fn disconnect(
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

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&req).await?
        };

        if !self.is_already_disconnected(&response) {
            assert_okay(response)?;
        }
        Ok(())
    }

    async fn inspect_network_by_hight<T>(&mut self, hight: &str) -> Result<T, DockerError>
    where
        T: DeserializeOwned,
    {
        let request = Request::Get {
            path: format!("/networks/{}", hight),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;

        Ok(from_value(json)?)
    }

    async fn find_connected_containers_by_hight(
        &mut self,
        hight: &str,
    ) -> Result<Vec<String>, DockerError> {
        let network = self
            .inspect_network_by_hight::<ConnectedContainers>(hight)
            .await?;

        Ok(network.container_ids)
    }

    fn is_already_disconnected(&mut self, response: &Response) -> bool {
        if let Response::Error {
            status: 500,
            body: Some(body),
            ..
        } = response
            && let Ok(json) = self.parser.parse(body.to_string())
            && let Ok(connection_error) = from_value::<ConnectionError>(json)
        {
            return connection_error.message.contains("not connected");
        }

        false
    }
}

#[cfg(test)]
mod tests {
    fn sorted_eq<T>(left: &[T], right: &[T]) -> bool
    where
        T: Clone + std::cmp::Ord,
    {
        let mut left_sorted = left.to_vec();
        let mut right_sorted = right.to_vec();
        left_sorted.sort();
        right_sorted.sort();
        left_sorted == right_sorted
    }

    mod key_deserializer {
        use crate::Deserialize;
        use crate::commands::network::deserialize_keys;
        use crate::commands::network::tests::sorted_eq;

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
                matches!(result, Ok(actual) if sorted_eq(&actual.keys, &["foo".to_string(), "bar".to_string()]))
            )
        }
    }

    mod inspect_by_id {
        use crate::{DockerError, commands::network::NetworkCommand};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command.inspect_by_id("").await;

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
        use crate::{DockerError, commands::network::NetworkCommand};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command.find_connected_containers_by_id("").await;

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
        use crate::{DockerError, commands::network::NetworkCommand};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command.find_connected_containers_by_name("").await;

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
        use crate::{DockerError, commands::network::NetworkCommand};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command.inspect_by_name("").await;

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
        use crate::{
            DockerError, Label,
            commands::network::{Network, NetworkCommand},
        };
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", Some(String::new()));

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_bad_json() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_okay("/networks/10111213", Some("oops".to_string()));

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/networks/10111213");

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .inspect_network_by_hight::<Network>("10111213")
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_inspect_by_id() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(
                    r#"{
                    "Id": "10111213",
                    "Name": "qux",
                    "Labels": {
                      "quux": "corge"
                    }
                }"#
                    .to_string(),
                ),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command.inspect_network_by_hight("10111213").await;

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
        use crate::{DockerError, commands::network::NetworkCommand};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/networks/10111213", Some(String::new()));

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/networks/10111213");

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_find_connected_containers_by_hight() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/networks/10111213",
                Some(
                    r#"{
                    "Containers": {
                        "123456": {}
                    }
                }"#
                    .to_string(),
                ),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .find_connected_containers_by_hight("10111213")
                .await;

            let expected = vec!["123456".to_string()];

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }

    mod create {
        use crate::{
            DockerError, Label,
            commands::network::{CreationBody, Network, NetworkCommand},
        };
        use simple_rest_client::{Header, MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .create("foo", vec![Label::new("bar", "baz")])
                .await;

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
                Some(String::new()),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .create("foo", vec![Label::new("bar", "baz")])
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
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
                Some(r#"{"Id":"123456","Warning":""}"#.to_string()),
            );

            mock_rest_client.expect_get_and_return_okay(
                "/networks/123456",
                Some(
                    r#"{
                    "Id": "123456",
                    "Name": "qux",
                    "Labels": {
                      "quux": "corge"
                      }
                }"#
                    .to_string(),
                ),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .create("foo", vec![Label::new("bar", "baz")])
                .await;
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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .create("foo", vec![Label::new("bar", "baz")])
                .await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
            let result = network_command
                .create("foo", vec![Label::new("bar", "baz")])
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }
    }

    mod connect {
        use crate::{
            Container, DockerError, Id, Image, Status,
            commands::network::{ConnectionBody, Network, NetworkCommand},
        };
        use simple_rest_client::{Header, MockRestClient, parsers::json::JsonParser};
        use std::{collections::HashSet, sync::Arc};

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
                Some(String::new()),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.connect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.connect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.connect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.connect(&network, &container).await;

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
                Some(String::new()),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.connect(&network, &container).await;
            assert!(matches!(result, Ok(())));
        }
    }

    mod disconnect {
        use crate::{
            Container, DockerError, Id, Image, Status,
            commands::network::{ConnectionBody, Network, NetworkCommand},
        };
        use simple_rest_client::{Header, MockRestClient, parsers::json::JsonParser};
        use std::{collections::HashSet, sync::Arc};

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;

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

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;
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
                Some(String::new()),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;
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
                Some(
                    r#"{"message":"container 654321 is not connected to the network foo"}"#
                        .to_string(),
                ),
            );

            let mut network_command = NetworkCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );
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

            let result = network_command.disconnect(&network, &container).await;
            assert!(matches!(result, Ok(())));
        }
    }
}
