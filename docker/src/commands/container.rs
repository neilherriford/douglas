use super::assert_non_empty_string_argument;
use crate::{
    Capability, Config, ContainerDefinition, ContainerUser, DockerError, EnvironmentVariable, Id,
    ImageIdentifier, Label, Mount, MountDefinition, Request, State, deserialize_id,
    serialize_capabilities, serialize_environment_variables, serialize_image_identifier,
    serialize_labels,
};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::{
    AssertionError, assert_created_with_body, assert_okay_with_body,
};
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Header, Response, RestClient, create_path_and_query_string};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize, PartialEq)]
struct Filter {
    pub name: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreationResponse {
    #[serde(rename = "Id")]
    id: String,
}

pub(crate) fn serialize_container_command<S>(
    command: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match command {
        Some(command) => {
            let tokens = shlex::split(command).unwrap();
            let mut seq = serializer.serialize_seq(Some(tokens.len()))?;
            for token in tokens {
                seq.serialize_element(&token)?;
            }
            seq.end()
        }
        None => unreachable!("skip_serializing_if should prevent None from reaching here"),
    }
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

#[derive(Debug, Serialize, PartialEq)]
struct HostConfig {
    #[serde(rename = "Mounts")]
    pub mounts: Vec<MountDefinition>,

    #[serde(rename = "CapAdd", serialize_with = "serialize_capabilities")]
    added_capabilities: Vec<Capability>,

    #[serde(rename = "GroupAdd")]
    pub additional_groups: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct CreateContainerBody {
    #[serde(rename = "User", skip_serializing_if = "Option::is_none")]
    pub run_as: Option<ContainerUser>,

    #[serde(rename = "Env", serialize_with = "serialize_environment_variables")]
    pub environment_variables: Vec<EnvironmentVariable>,

    #[serde(
        rename = "Cmd",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_container_command"
    )]
    pub command: Option<String>,

    #[serde(rename = "Image", serialize_with = "serialize_image_identifier")]
    pub image_identifier: ImageIdentifier,

    #[serde(rename = "HostConfig")]
    host_config: HostConfig,

    #[serde(rename = "Labels", serialize_with = "serialize_labels")]
    labels: Vec<Label>,
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
            0 => Err(DockerError::ResourceNotFound),
            1 => self.find_by_id(&identified_containers[0].id).await,
            _ => Err(DockerError::AmbiguousMatchError),
        }
    }

    pub async fn create(
        &self,
        definition: ContainerDefinition,
    ) -> Result<InspectedContainer, DockerError> {
        let request_body = CreateContainerBody {
            run_as: definition.run_as,
            environment_variables: definition.environment_variables,
            command: definition.command,
            image_identifier: definition.image_name,
            host_config: HostConfig {
                mounts: definition.mounts,
                added_capabilities: definition.added_capabilities,
                additional_groups: vec!["1000".into()],
            },
            labels: definition.labels,
        };

        let request = Request::Post {
            path: create_path_and_query_string(
                "/containers/create",
                HashMap::from([("name", definition.name.as_str())]),
            ),
            headers: vec![Header::content_type_json()],
            body: Some(serde_json::to_string(&request_body)?),
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&request).await?
        };

        let body = assert_created_with_body(response)?;
        let json = self.parser.parse(body)?;
        let result: CreationResponse = from_value(json)?;

        self.find_by_id(&result.id).await
    }

    pub async fn start(&self, id: &str) -> Result<(), DockerError> {
        let request = Request::Post {
            path: format!("/containers/{id}/start"),
            headers: vec![],
            body: None,
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&request).await?
        };

        match response {
            Response::Okay { headers: _, body } => Err(AssertionError::UnexpectedResponseError {
                status: 200,
                body,
                message: "expected NO CONTENT, but recieved OK".to_string(),
            }
            .into()),
            Response::Created { headers: _, body } => {
                Err(AssertionError::UnexpectedResponseError {
                    status: 200,
                    body,
                    message: "expected NO CONTENT, but recieved CREATED".to_string(),
                }
                .into())
            }
            Response::NoContent { .. } => Ok(()),
            Response::Error { status: 304, .. } => Ok(()),
            Response::Error { status: 404, .. } => Err(AssertionError::NotFoundError.into()),
            Response::Error { status, body, .. } => Err(AssertionError::UnexpectedResponseError {
                status,
                body,
                message: "non successful response".to_string(),
            }
            .into()),
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
            commands::container::{ContainerCommand, InspectedContainer},
        };
        use std::sync::Arc;

        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};

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
                Err(DockerError::ResponseError { status: 201, .. })
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
                Err(DockerError::ResponseError { status: 204, .. })
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

            assert!(matches!(result, Err(DockerError::ResourceNotFound)));
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
                Err(DockerError::ResponseError { status: 500, .. })
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
            commands::container::{ContainerCommand, InspectedContainer},
        };
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
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
                Err(DockerError::ResponseError { status: 201, .. })
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
                Err(DockerError::ResponseError { status: 204, .. })
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
                Err(DockerError::ResponseError { status: 500, .. })
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
