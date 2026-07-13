use super::assert_non_empty_string_argument;
use crate::{
    Capability, Config, ContainerDefinition, ContainerName, ContainerUser, DockerError,
    EnvironmentVariable, Id, ImageIdentifier, Label, Mount, MountDefinition, Request, State,
    deserialize_id, serialize_capabilities, serialize_environment_variables,
    serialize_image_identifier, serialize_labels,
};
use log::{Reporter, Span};
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
    reporter: Arc<dyn Reporter>,
}

impl ContainerCommand {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
        }
    }

    pub async fn find_by_id(&self, id: &str) -> Result<InspectedContainer, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find container by id",
            log::ScopeKind::Task,
        )
        .start_guard();
        assert_non_empty_string_argument("id", id)?;

        let request = Request::Get {
            path: format!("/containers/{}/json", id),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        Ok(guard.finish(from_value(json))?)
    }

    pub async fn find_by_name(&self, name: &ContainerName) -> Result<InspectedContainer, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find container by name",
            log::ScopeKind::Task,
        )
        .start_guard();

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
            rest_client.execute(guard.span(), &request).await?
        };
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        let identified_containers: Vec<IdentifiedContainer> = from_value(json)?;

        guard.finish(match identified_containers.len() {
            0 => Err(DockerError::ResourceNotFound),
            1 => self.find_by_id(&identified_containers[0].id).await,
            _ => Err(DockerError::AmbiguousMatchError),
        })
    }

    pub async fn create(
        &self,
        definition: ContainerDefinition,
    ) -> Result<InspectedContainer, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Create container",
            log::ScopeKind::Task,
        )
        .start_guard();

        let request_body = CreateContainerBody {
            run_as: definition.run_as,
            environment_variables: definition.environment_variables,
            command: definition.command,
            image_identifier: definition.image_name,
            host_config: HostConfig {
                mounts: definition.mounts,
                added_capabilities: definition.added_capabilities,
                additional_groups: vec![], // TODO
            },
            labels: definition.labels,
        };

        let request = Request::Post {
            path: create_path_and_query_string(
                "/containers/create",
                HashMap::from([("name", definition.name.as_ref())]),
            ),
            headers: vec![Header::content_type_json()],
            body: Some(serde_json::to_string(&request_body)?),
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(guard.span(), &request).await?
        };

        let body = assert_created_with_body(response)?;
        let json = self.parser.parse(body)?;
        let result: CreationResponse = from_value(json)?;

        self.find_by_id(&result.id).await
    }

    pub async fn start(&self, id: &str) -> Result<(), DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Start container",
            log::ScopeKind::Task,
        )
        .start_guard();

        let request = Request::Post {
            path: format!("/containers/{id}/start"),
            headers: vec![],
            body: None,
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(guard.span(), &request).await?
        };

        guard.finish(match response {
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
        })
    }
}
