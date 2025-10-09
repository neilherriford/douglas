use super::json_parser::JsonParserError;
use super::{assert_non_empty_string_argument, assert_okay_with_body};
use crate::{Config, DockerError, Id, Mount, Parser, Request, State, deserialize_id};
use serde::{Deserialize, Serialize};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::{RestClient, create_path_and_query_string};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize, PartialEq)]
struct Filter {
    pub name: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct InspectedContainer {
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

    #[serde(rename = "Config")]
    pub config: Config,
}

#[derive(Debug, Deserialize, PartialEq)]
struct IdentifiedContainer {
    #[serde(rename = "Id")]
    pub id: String,
}

pub struct ContainerCommand {
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
}

impl ContainerCommand {
    pub fn new(
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            rest_client,
            parser,
        }
    }

    pub async fn find_by_id(&self, id: &str) -> Result<InspectedContainer, DockerError> {
        assert_non_empty_string_argument("id", id)?;

        let request = Request::Get {
            path: format!("/containers/{}/json", id),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        Ok(from_value(json)?)
    }

    pub async fn find_by_name(&self, name: &str) -> Result<InspectedContainer, DockerError> {
        let filter = serde_json::to_string(&Filter {
            name: vec![name.to_string()],
        })?;
        let request = Request::Get {
            path: create_path_and_query_string(
                "/containers/json",
                HashMap::from([("all", "true"), ("filters", &filter)]),
            ),
            headers: vec![],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&request).await?
        };
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        let identified_containers: Vec<IdentifiedContainer> = from_value(json)?;

        match identified_containers.len() {
            0 => Err(DockerError::NotFoundError),
            1 => self.find_by_id(&identified_containers[0].id).await,
            _ => Err(DockerError::AmbiguousMatchError),
        }
    }
}

#[cfg(test)]
mod tests {
    mod environment_variables_deserializer {
        use crate::{EnvironmentVariable, deserialize_environment_variables};
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
        use crate::{
            Config, DockerError, EnvironmentVariable, Id, Label, Mount, MountType, State, Status,
            commands::{
                container::{ContainerCommand, InspectedContainer},
                json_parser::JsonParser,
            },
        };
        use std::sync::Arc;

        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_id_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("").await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "id" && given == String::new()
            ));
        }

        #[tokio::test]
        async fn should_error_if_body_empty() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_okay("/containers/123456/json", Some(String::new()));

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/containers/123456/json");

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/containers/123456/json");

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/containers/123456/json");

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/containers/123456/json");

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;

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
                Some(
                    r#"{
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
                    "#
                    .to_string(),
                ),
            );

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_id("123456").await;
            let expected = InspectedContainer {
                id: "123456".to_string(),
                name: "foo".to_string(),
                state: State {
                    status: Status::Exited,
                },
                image_id: Id {
                    algorithm: "alg".to_string(),
                    hex: "654321".to_string(),
                },
                mounts: vec![Mount {
                    mount_type: MountType::Bind,
                    source: "/bar/".to_string(),
                    destination: "/baz/".to_string(),
                    writable: true,
                }],
                config: Config {
                    env: vec![EnvironmentVariable {
                        name: "quux".to_string(),
                        value: "corge".to_string(),
                    }],
                    labels: vec![Label {
                        name: "grault".to_string(),
                        value: "garply".to_string(),
                    }],
                },
            };

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }

    mod find_by_name {
        use crate::{
            Config, DockerError, EnvironmentVariable, Id, Label, Mount, MountType, State, Status,
            commands::{
                container::{ContainerCommand, InspectedContainer},
                json_parser::JsonParser,
            },
        };
        use simple_rest_client::MockRestClient;
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
                Some(String::new()),
            );

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_name("foo").await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none(
                "/containers/json?all=true&filters=%7B%22name%22%3A%5B%22foo%22%5D%7D",
            );

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_name("foo").await;

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

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_name("foo").await;

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

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_name("foo").await;

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
                Some(r#"[{ "Id": "123456" }]"#.to_string()),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/containers/123456/json",
                Some(
                    r#"{
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
                    "#
                    .to_string(),
                ),
            );

            let command = ContainerCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
            );

            let result = command.find_by_name("foo").await;

            let expected = InspectedContainer {
                id: "123456".to_string(),
                name: "foo".to_string(),
                state: State {
                    status: Status::Exited,
                },
                image_id: Id {
                    algorithm: "alg".to_string(),
                    hex: "654321".to_string(),
                },
                mounts: vec![Mount {
                    mount_type: MountType::Bind,
                    source: "/bar/".to_string(),
                    destination: "/baz/".to_string(),
                    writable: true,
                }],
                config: Config {
                    env: vec![EnvironmentVariable {
                        name: "quux".to_string(),
                        value: "corge".to_string(),
                    }],
                    labels: vec![Label {
                        name: "grault".to_string(),
                        value: "garply".to_string(),
                    }],
                },
            };

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }
}
