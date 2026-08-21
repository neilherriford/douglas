use crate::client::ContainerRef;
use crate::{
    Config, DockerError, EnvironmentVariable, Id, Label, Mount, Request, State, commands,
    deserialize_id, to_general_error,
};
use docker_types::{
    Capability, ContainerDefinition, ContainerId, ContainerRuntimeState, ContainerSnapshot,
    ContainerUser, MountDefinition, MountName, NewContainer, PortMapping, Registry, Status,
    deserialize_labels, serialize_environment_variables, serialize_labels,
};
use log::{Reporter, Span};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::{
    assert_created_with_body, assert_no_content, assert_okay_with_body,
};
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Header, RestClient, create_path_and_query_string};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Deserialize, PartialEq)]
struct CreationResponse {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Serialize)]
struct CreateContainerHostConfig {
    #[serde(rename = "Mounts")]
    mounts: Vec<MountDefinition>,
    #[serde(rename = "CapAdd")]
    added_capabilities: HashSet<Capability>,
    #[serde(rename = "PortBindings", serialize_with = "serialize_port_bindings")]
    published_ports: Vec<PortMapping>,
}

#[derive(Debug, Serialize)]
struct CreateContainerRequest {
    #[serde(rename = "Image")]
    image: String,
    #[serde(
        rename = "Cmd",
        serialize_with = "serialize_container_command",
        skip_serializing_if = "Option::is_none"
    )]
    command: Option<String>,
    #[serde(rename = "Env", serialize_with = "serialize_environment_variables")]
    environment_variables: Vec<EnvironmentVariable>,
    #[serde(rename = "Labels", serialize_with = "serialize_labels")]
    labels: Vec<Label>,
    #[serde(rename = "User", skip_serializing_if = "Option::is_none")]
    run_as: Option<ContainerUser>,
    #[serde(rename = "ExposedPorts", serialize_with = "serialize_exposed_ports")]
    published_ports: Vec<PortMapping>,
    #[serde(rename = "HostConfig")]
    host_config: CreateContainerHostConfig,
}

impl CreateContainerRequest {
    fn build(registry: &Registry, new_container: &NewContainer) -> Self {
        Self {
            image: format!(
                "{registry}/{}",
                new_container.image.version_formatted_name()
            ),
            command: new_container.command.clone(),
            environment_variables: new_container.environment_variables.clone(),
            labels: new_container.labels.clone(),
            run_as: new_container.run_as.clone(),
            published_ports: new_container.published_ports.clone(),
            host_config: CreateContainerHostConfig {
                mounts: new_container.mounts.clone(),
                added_capabilities: new_container.added_capabilities.clone(),
                published_ports: new_container.published_ports.clone(),
            },
        }
    }
}

fn serialize_exposed_ports<S>(
    published_ports: &[PortMapping],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(published_ports.len()))?;
    for port in published_ports {
        map.serialize_entry(
            &format!("{}/tcp", port.container_port),
            &Json::Object(serde_json::Map::new()),
        )?;
    }
    map.end()
}

fn serialize_port_bindings<S>(
    published_ports: &[PortMapping],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(published_ports.len()))?;
    for port in published_ports {
        map.serialize_entry(
            &format!("{}/tcp", port.container_port),
            &[HostPortBinding {
                host_ip: "127.0.0.1",
                host_port: port.host_port.to_string(),
            }],
        )?;
    }
    map.end()
}

#[derive(Debug, Serialize)]
struct HostPortBinding {
    #[serde(rename = "HostIp")]
    host_ip: &'static str,
    #[serde(rename = "HostPort")]
    host_port: String,
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
pub struct InspectedContainer {
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

    #[serde(rename = "HostConfig")]
    pub host_config: InspectedHostConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
struct InspectedContainerLabels {
    #[serde(rename = "Config")]
    pub config: InspectedContainerConfigLabelsOnly,
}

#[derive(Debug, Deserialize, PartialEq)]
struct InspectedContainerConfigLabelsOnly {
    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct InspectedContainerState {
    #[serde(rename = "State")]
    pub state: State,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct InspectedHostConfig {
    #[serde(rename = "CapAdd")]
    pub added_capabilities: HashSet<Capability>,
}

fn to_mount_definition(
    mount: Mount,
    mount_names: &HashMap<PathBuf, MountName>,
) -> Result<MountDefinition, DockerError> {
    let container_path = PathBuf::from(&mount.destination);
    let name = mount_names
        .get(&container_path)
        .cloned()
        .ok_or_else(|| DockerError::UnknownMount(container_path.clone()))?;

    Ok(MountDefinition {
        name,
        host_path: PathBuf::from(&mount.source),
        container_path,
        writable: mount.writable,
    })
}

pub async fn exists(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
) -> Result<bool, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Find container",
        log::ScopeKind::Task,
    )
    .start_guard();
    let json = find_container(rest_client, &parser, container_ref, &guard).await?;

    if let Some(array) = json.as_array() {
        match array.len() {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DockerError::AmbiguousMatchError),
        }
    } else {
        Err(DockerError::GeneralError("Unexpected response".to_string()))
    }
}

pub async fn labels(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
) -> Result<Vec<Label>, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Find container",
        log::ScopeKind::Task,
    )
    .start_guard();
    let json = inspect_container(rest_client, &parser, &container_ref, &guard).await?;
    let buffer: InspectedContainerLabels = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(buffer.config.labels))
}

#[derive(Debug, Deserialize)]
struct ContainerListEntry {
    #[serde(rename = "Names")]
    names: Vec<String>,
}

pub async fn list(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
) -> Result<Vec<docker_types::ContainerName>, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Listing containers",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Get {
        path: create_path_and_query_string("/containers/json", HashMap::from([("all", "true")])),
        headers: vec![],
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let entries: Vec<ContainerListEntry> = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(entries
        .into_iter()
        .filter_map(|entry| entry.names.into_iter().next())
        .filter_map(|name| name.trim_start_matches('/').parse().ok())
        .collect()))
}

pub async fn find_id(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
) -> Result<String, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Find container id",
        log::ScopeKind::Task,
    )
    .start_guard();

    let json = inspect_container(rest_client, &parser, &container_ref, &guard).await?;
    let buffer: InspectedContainer = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(buffer.id))
}

pub async fn find(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
    mount_names: &HashMap<PathBuf, MountName>,
) -> Result<ContainerSnapshot, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Find container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let json = inspect_container(rest_client, &parser, &container_ref, &guard).await?;
    let buffer: InspectedContainer = from_value(json).map_err(to_general_error)?;

    let name = buffer.name.trim_start_matches('/').parse().map_err(
        |err: docker_types::DockerNameError| DockerError::InvalidName {
            name: buffer.name.clone(),
            description: err.to_string(),
        },
    )?;
    let image =
        commands::image::inspect(Arc::clone(&reporter), rest_client, parser, buffer.image_id)
            .await?;
    let mounts = buffer
        .mounts
        .into_iter()
        .map(|mount| to_mount_definition(mount, mount_names))
        .collect::<Result<Vec<_>, _>>()?;

    guard.finish(Ok(ContainerSnapshot {
        definition: ContainerDefinition {
            name,
            command: buffer.config.command,
            environment_variables: buffer.config.env,
            image,
            mounts,
            added_capabilities: buffer.host_config.added_capabilities.clone(),
            labels: buffer.config.labels,
        },
        runtime_state: ContainerRuntimeState {
            status: buffer.state.status,
            started_at: buffer.state.started_at,
            health_status: buffer.state.health.map(|health| health.status),
        },
    }))
}

async fn inspect_container(
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: &Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: &ContainerRef,
    guard: &log::ScopeGuard,
) -> Result<Json, DockerError> {
    let request = Request::Get {
        path: format!("/containers/{container_ref}/json"),
        headers: vec![],
    };
    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    Ok(json)
}

async fn find_container(
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: &Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
    guard: &log::ScopeGuard,
) -> Result<Json, DockerError> {
    let (filter_key, filter_value): (&str, String) = match container_ref {
        ContainerRef::FullName(name) => ("name", name.to_string()),
        ContainerRef::ContainerId(ContainerId::Full(id)) => ("id", id.to_string()),
        ContainerRef::ContainerId(ContainerId::Short(id)) => ("id", id.to_string()),
    };
    let filters = serde_json::to_string(&HashMap::from([(filter_key, vec![filter_value])]))
        .map_err(to_general_error)?;
    let request = Request::Get {
        path: create_path_and_query_string(
            "/containers/json",
            HashMap::from([("all", "true"), ("filters", filters.as_str())]),
        ),
        headers: vec![],
    };
    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    Ok(json)
}

pub async fn delete(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    container_ref: ContainerRef,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Deleting container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Delete {
        path: create_path_and_query_string(
            &format!("/containers/{container_ref}"),
            HashMap::from([("v", "true")]),
        ),
        headers: vec![],
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    assert_no_content(response)?;

    guard.finish(Ok(()))
}

pub async fn start(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    container_ref: ContainerRef,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Post {
        path: create_path_and_query_string(
            &format!("/containers/{container_ref}/start"),
            HashMap::new(),
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
    assert_no_content(response)?;

    guard.finish(Ok(()))
}

pub async fn stop(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    container_ref: ContainerRef,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Stopping container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Post {
        path: create_path_and_query_string(
            &format!("/containers/{container_ref}/stop"),
            HashMap::new(),
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
    assert_no_content(response)?;

    guard.finish(Ok(()))
}

pub async fn status(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: ContainerRef,
) -> Result<Status, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Container stautus",
        log::ScopeKind::Task,
    )
    .start_guard();

    let json = inspect_container(rest_client, &parser, &container_ref, &guard).await?;
    let buffer: InspectedContainerState = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(buffer.state.status))
}

pub async fn create(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    registry: &Registry,
    new_container: &NewContainer,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Creating container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let body = serde_json::to_string(&CreateContainerRequest::build(registry, new_container))
        .map_err(to_general_error)?;

    let request = Request::Post {
        path: create_path_and_query_string(
            "/containers/create",
            HashMap::from([("name", new_container.name.as_ref())]),
        ),
        headers: vec![Header::content_type_json()],
        body: Some(body),
    };
    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_created_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let _created: CreationResponse = from_value(json).map_err(to_general_error)?;

    guard.finish_with_outcome(log::Outcome::Ok);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docker_types::VersionedImageName;
    use serde_json::{Value, json};

    fn new_container() -> NewContainer {
        NewContainer {
            name: "traefik".parse().unwrap(),
            image: VersionedImageName::specific("traefik", "v3.7.7"),
            run_as: None,
            command: None,
            environment_variables: Vec::new(),
            mounts: Vec::new(),
            added_capabilities: HashSet::new(),
            labels: Vec::new(),
            published_ports: Vec::new(),
        }
    }

    #[test]
    fn test_build_should_qualify_the_image_with_the_registry() {
        let registry: Registry = "localhost:7376".parse().unwrap();

        let request = CreateContainerRequest::build(&registry, &new_container());

        assert_eq!(request.image, "localhost:7376/traefik:v3.7.7");
    }

    #[test]
    fn test_build_should_serialize_the_registry_qualified_image_as_the_image_field() {
        let registry: Registry = "localhost:7376".parse().unwrap();

        let json_str =
            serde_json::to_string(&CreateContainerRequest::build(&registry, &new_container()))
                .unwrap();
        let actual: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(actual["Image"], json!("localhost:7376/traefik:v3.7.7"));
    }

    #[test]
    fn test_build_should_omit_exposed_ports_and_bindings_when_no_ports_are_published() {
        let registry: Registry = "localhost:7376".parse().unwrap();

        let json_str =
            serde_json::to_string(&CreateContainerRequest::build(&registry, &new_container()))
                .unwrap();
        let actual: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(actual["ExposedPorts"], json!({}));
        assert_eq!(actual["HostConfig"]["PortBindings"], json!({}));
    }

    #[test]
    fn test_build_should_expose_and_bind_published_ports_to_localhost() {
        let registry: Registry = "localhost:7376".parse().unwrap();
        let mut container = new_container();
        container.published_ports = vec![PortMapping {
            host_port: 80,
            container_port: 80,
        }];

        let json_str =
            serde_json::to_string(&CreateContainerRequest::build(&registry, &container)).unwrap();
        let actual: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(actual["ExposedPorts"], json!({"80/tcp": {}}));
        assert_eq!(
            actual["HostConfig"]["PortBindings"],
            json!({"80/tcp": [{"HostIp": "127.0.0.1", "HostPort": "80"}]})
        );
    }

    #[test]
    fn test_to_mount_definition_should_resolve_the_configured_mount_name() {
        let container_path = PathBuf::from("/etc/traefik");
        let mount_names = HashMap::from([(
            container_path.clone(),
            "shared".parse::<MountName>().unwrap(),
        )]);
        let mount = Mount {
            mount_type: docker_types::MountType::Bind,
            source: "/var/lib/douglas/mounts/traefik/shared".to_string(),
            destination: "/etc/traefik".to_string(),
            writable: false,
        };

        let result = to_mount_definition(mount, &mount_names).expect("should resolve");

        assert_eq!(result.name.as_ref(), "shared");
        assert_eq!(result.container_path, container_path);
        assert!(!result.writable);
    }

    #[test]
    fn test_to_mount_definition_should_error_on_an_unrecognized_mount_path() {
        let mount = Mount {
            mount_type: docker_types::MountType::Bind,
            source: "/var/lib/douglas/mounts/traefik/shared".to_string(),
            destination: "/etc/unexpected".to_string(),
            writable: false,
        };

        let result = to_mount_definition(mount, &HashMap::new());

        assert!(matches!(result, Err(DockerError::UnknownMount(_))));
    }
}
