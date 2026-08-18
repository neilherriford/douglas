use super::assert_no_docker_errors;
use crate::DockerError;
use crate::client::ImageRef;
use crate::{deserialize_container_command, deserialize_run_as, to_general_error};
use docker_types::{
    ContainerUser, EnvironmentVariable, Healthcheck, Id, ImageDefinition, ImageId, Label, Registry,
    VersionedImageName, deserialize_environment_variables, deserialize_id, deserialize_labels,
};
use log::{Reporter, Span};
use serde::{Deserialize, Deserializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::assert_okay_with_body;
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Request, RestClient, create_path_and_query_string};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct ImageSummary {
    #[serde(rename = "Id")]
    #[serde(deserialize_with = "deserialize_id")]
    id: Id,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct InspectedImage {
    #[serde(rename = "Id")]
    #[serde(deserialize_with = "deserialize_id")]
    pub id: Id,

    #[serde(rename = "Created")]
    pub created: String,

    #[serde(rename = "Architecture")]
    pub architecture: String,

    #[serde(rename = "Config")]
    pub config: InspectedImageConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct InspectedImageConfig {
    #[serde(rename = "User")]
    #[serde(deserialize_with = "deserialize_run_as")]
    pub run_as: Option<ContainerUser>,

    #[serde(rename = "ExposedPorts")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_exposed_ports")]
    pub exposed_ports: Vec<String>,

    #[serde(rename = "Env")]
    #[serde(deserialize_with = "deserialize_environment_variables")]
    pub env: Vec<EnvironmentVariable>,

    #[serde(rename = "Cmd")]
    #[serde(deserialize_with = "deserialize_container_command")]
    pub command: Option<String>,

    #[serde(rename = "Healthcheck")]
    pub health_check: Option<Healthcheck>,

    #[serde(rename = "Volumes")]
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_volumes")]
    pub volumes: Vec<PathBuf>,

    #[serde(rename = "WorkingDir")]
    pub working_dir: String,

    #[serde(rename = "Entrypoint")]
    #[serde(deserialize_with = "deserialize_container_command")]
    pub entrypoint: Option<String>,

    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

fn deserialize_exposed_ports<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<HashMap<String, serde_json::Value>> = Option::deserialize(deserializer)?;
    Ok(raw
        .map(|ports| ports.into_keys().collect())
        .unwrap_or_default())
}

fn deserialize_volumes<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<HashMap<String, serde_json::Value>> = Option::deserialize(deserializer)?;
    Ok(raw
        .map(|volumes| volumes.into_keys().map(PathBuf::from).collect())
        .unwrap_or_default())
}

impl From<InspectedImage> for ImageDefinition {
    fn from(image: InspectedImage) -> Self {
        ImageDefinition {
            id: image.id,
            created: image.created,
            architecture: image.architecture,
            run_as: image.config.run_as,
            exposed_ports: image.config.exposed_ports,
            environment_variables: image.config.env,
            command: image.config.command,
            health_check: image.config.health_check,
            volumes: image.config.volumes,
            working_dir: image.config.working_dir,
            entrypoint: image.config.entrypoint,
            labels: image.config.labels,
        }
    }
}

pub async fn find(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    registry: &Registry,
    image_ref: ImageRef,
) -> Result<ImageDefinition, DockerError> {
    let guard = Span::new(Arc::clone(&reporter), "Find image", log::ScopeKind::Task).start_guard();

    let json = find_image(rest_client, &parser, registry, image_ref, &guard).await?;
    let mut summaries: Vec<ImageSummary> = from_value(json).map_err(to_general_error)?;

    let id = match summaries.len() {
        0 => return guard.finish(Err(DockerError::ResourceNotFound)),
        1 => summaries.remove(0).id,
        _ => return guard.finish(Err(DockerError::AmbiguousMatchError)),
    };

    guard.finish(inspect(reporter, rest_client, parser, id).await)
}

pub async fn exists(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    registry: &Registry,
    image_ref: ImageRef,
) -> Result<bool, DockerError> {
    let guard =
        Span::new(Arc::clone(&reporter), "Image exists?", log::ScopeKind::Task).start_guard();

    let json = find_image(rest_client, &parser, registry, image_ref, &guard).await?;
    guard.finish(if let Some(array) = json.as_array() {
        match array.len() {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DockerError::AmbiguousMatchError),
        }
    } else {
        Err(DockerError::GeneralError(
            "Expected response body to be an array".to_string(),
        ))
    })
}

async fn find_image(
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: &Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    registry: &Registry,
    image_ref: ImageRef,
    guard: &log::ScopeGuard,
) -> Result<Json, DockerError> {
    let (filter_key, filter_value): (&str, String) = match image_ref {
        ImageRef::VersionedName(name) => (
            "reference",
            format!("{registry}/{}", name.version_formatted_name()),
        ),
        ImageRef::ImageId(ImageId::Full(id)) => ("id", id.to_string()),
        ImageRef::ImageId(ImageId::Short(id)) => ("id", id.to_string()),
    };
    let filters = serde_json::to_string(&HashMap::from([(filter_key, vec![filter_value])]))
        .map_err(to_general_error)?;
    let request = Request::Get {
        path: create_path_and_query_string(
            "/images/json",
            HashMap::from([("all", "true"), ("filters", filters.as_str())]),
        ),
        headers: vec![],
    };
    let mut rest_client = rest_client.lock().await;
    let response = rest_client
        .execute(guard.span(), &request)
        .await
        .map_err(to_general_error)?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    Ok(json)
}

pub async fn inspect(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    id: Id,
) -> Result<ImageDefinition, DockerError> {
    let guard =
        Span::new(Arc::clone(&reporter), "Inspect image", log::ScopeKind::Task).start_guard();

    let request = Request::Get {
        path: format!("/images/{id}/json"),
        headers: vec![],
    };

    let mut rest_client = rest_client.lock().await;
    let response = rest_client
        .execute(guard.span(), &request)
        .await
        .map_err(to_general_error)?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let result: InspectedImage = from_value(json).map_err(to_general_error)?;
    Ok(result.into())
}

pub async fn pull(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
    registry: &Registry,
    versioned_image_name: &VersionedImageName,
) -> Result<ImageDefinition, DockerError> {
    let guard = Span::new(Arc::clone(&reporter), "Pull image", log::ScopeKind::Task).start_guard();

    let from_image = format!("{registry}/{}", versioned_image_name.formatted_name());
    let request = Request::Post {
        path: create_path_and_query_string(
            "/images/create",
            HashMap::from([
                ("fromImage", from_image.as_str()),
                ("tag", &versioned_image_name.version.to_string()),
            ]),
        ),
        headers: vec![],
        body: None,
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let chunks = chunked_parser.parse(body).map_err(to_general_error)?;
    assert_no_docker_errors(chunks)?;

    guard.finish(
        find(
            reporter,
            rest_client,
            parser,
            registry,
            ImageRef::VersionedName(versioned_image_name.clone()),
        )
        .await,
    )
}
