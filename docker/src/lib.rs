mod commands;

use async_trait::async_trait;
use commands::ImageCommand;
use commands::container::{ContainerCommand, InspectedContainer};
use commands::json_parser::ChunkedJsonParser;
use commands::network::{Network, NetworkCommand};
use commands::ping::{PingCommand, PingParser};
use file_system::{FileSystemError, path_to_string};
use log::Logger;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::Value as Json;
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::{JsonParser, JsonParserError};
use simple_rest_client::unix_domain_socket::{BuilderError, build_client};
use simple_rest_client::{Request, RestClient, RestClientError};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Capability {
    IpcLock,
    Chown,
}

pub(crate) fn serialize_capabilities<S>(
    capabilities: &Vec<Capability>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(capabilities.len()))?;
    for capability in capabilities {
        let text = match capability {
            Capability::IpcLock => "IPC_LOCK",
            Capability::Chown => "CAP_CHOWN",
        };

        seq.serialize_element(&text)?;
    }
    seq.end()
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Id {
    pub algorithm: String,
    pub hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Label {
    pub name: String,
    pub value: String,
}

impl Label {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

pub(crate) fn deserialize_labels<'de, D>(deserializer: D) -> Result<Vec<Label>, D::Error>
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

pub(crate) fn serialize_labels<S>(labels: &Vec<Label>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(labels.len()))?;
    for label in labels {
        map.serialize_entry(&label.name, &label.value)?;
    }
    map.end()
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

pub(crate) fn deserialize_id<'de, D>(deserializer: D) -> Result<Id, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_id: String = Deserialize::deserialize(deserializer)?;
    let parts: Vec<&str> = raw_id.split(':').collect();
    let (algorithm, hex) = match parts.as_slice() {
        [first, second] => (first.to_string(), second.to_string()),
        _ => ("missing-algorithim".to_string(), raw_id),
    };

    Ok(Id { algorithm, hex })
}

#[derive(Error, Debug)]
pub enum DockerError {
    #[error("Received unexpected response with status: {status}, {message}")]
    UnexpectedResponseError {
        status: u16,
        body: Option<String>,
        message: String,
    },

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Init error: {0}")]
    InitError(#[from] BuilderError),

    #[error("Ambiguous match")]
    AmbiguousMatchError,

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("API error {0}")]
    ApiError(String),

    #[error("Not found")]
    NotFoundError,

    #[error("Invalid argument, '{name}: {given}' {message}")]
    InvalidArgumentError {
        name: String,
        given: String,
        message: String,
    },

    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),

    #[error("Invalid path")]
    PathError { path: PathBuf, message: String },
}

impl PartialEq for DockerError {
    fn eq(&self, other: &Self) -> bool {
        match self {
            DockerError::IoError(left) => {
                if let DockerError::IoError(right) = other {
                    left.to_string() == right.to_string()
                } else {
                    false
                }
            }
            _ => self == other,
        }
    }
}

impl From<serde_json::Error> for DockerError {
    fn from(err: serde_json::Error) -> DockerError {
        DockerError::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Version {
    Latest,
    Specific(String),
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let formatted = match self {
            Version::Latest => "latest".to_string(),
            Version::Specific(version) => version.to_string(),
        };

        write!(f, "{formatted}")
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct ImageName {
    pub namespace: String,
    pub name: String,
    pub version: Version,
}

impl ImageName {
    pub fn latest(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            name: name.to_string(),
            version: Version::Latest,
        }
    }
    pub fn specific(namespace: &str, name: &str, version: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            name: name.to_string(),
            version: Version::Specific(version.to_string()),
        }
    }
}

impl std::fmt::Display for ImageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "{}/{}:{}",
            self.namespace, self.name, self.version
        ))
    }
}

pub fn serialize_image_identifier<S>(
    image_identifier: &ImageIdentifier,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let id = match image_identifier {
        ImageIdentifier::Id(id) => id.to_string(),
        ImageIdentifier::ImageName(image_name) => image_name.to_string(),
    };
    serializer.collect_str(&id)
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct Tag {
    pub name: String,
    pub version: String,
}

#[derive(Debug, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: Image,
    pub status: Status,
    pub mounts: Vec<Mount>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub labels: Vec<Label>,
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

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let text = match self {
            Status::Created => "created",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Restarting => "restarting",
            Status::Removing => "removing",
            Status::Exited => "exited",
            Status::Dead => "dead",
        };
        write!(f, "{text}")
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

impl std::fmt::Display for MountType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let text = match self {
            MountType::Bind => "bind",
            MountType::Volume => "volume",
            MountType::Image => "image",
            MountType::Tmpfs => "tmpfs",
            MountType::Npipe => "npipe",
            MountType::Cluster => "cluster",
        };
        write!(f, "{text}")
    }
}

#[derive(Debug, PartialEq, Ord, PartialOrd, Eq, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
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

pub(crate) fn serialize_environment_variables<S>(
    environment_variables: &Vec<EnvironmentVariable>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(environment_variables.len()))?;
    for environment_variable in environment_variables {
        let s = format!(
            "{}={}",
            environment_variable.name, environment_variable.value
        );
        seq.serialize_element(&s)?;
    }
    seq.end()
}

impl EnvironmentVariable {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Image {
    #[serde(rename = "Id")]
    #[serde(deserialize_with = "deserialize_id")]
    pub id: Id,

    #[serde(rename = "RepoTags")]
    #[serde(deserialize_with = "deserialize_tags")]
    pub tags: HashSet<Tag>,
}

fn deserialize_tags<'de, D>(deserializer: D) -> Result<HashSet<Tag>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_strings: Vec<String> = Deserialize::deserialize(deserializer)?;

    let tags = raw_strings
        .into_iter()
        .map(|raw_tag| {
            let parts: Vec<&str> = raw_tag.split(':').collect();
            let (name, version) = match parts.as_slice() {
                [first, second] => (first.to_string(), second.to_string()),
                _ => (raw_tag, String::from("")),
            };
            Tag { name, version }
        })
        .collect();

    Ok(tags)
}

pub enum PingResult {
    Ok,
    Error(String),
}

#[async_trait]
pub trait SystemClient {
    async fn ping(&mut self) -> Result<PingResult, DockerError>;
}

pub struct SimpleSystemClient {
    command: PingCommand,
}

impl SimpleSystemClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let rest_client = build_client(socket_file_path, logger).await?;

        Ok(Self {
            command: PingCommand::new(
                Arc::new(tokio::sync::Mutex::new(rest_client)),
                Box::new(PingParser::new()),
            ),
        })
    }
}

#[async_trait]
impl SystemClient for SimpleSystemClient {
    async fn ping(&mut self) -> Result<PingResult, DockerError> {
        Ok(self.command.ping().await?)
    }
}

#[async_trait]
pub trait ImageClient {
    async fn find_by_id(&mut self, id: &Id) -> Result<Image, DockerError>;
    async fn find_by_name(&mut self, image_name: &ImageName) -> Result<Image, DockerError>;
    async fn list(&mut self) -> Result<Vec<Image>, DockerError>;
    async fn pull(&mut self, image_name: &ImageName) -> Result<Image, DockerError>;
}

pub struct SimpleImageClient {
    command: ImageCommand,
}

impl SimpleImageClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let rest_client = build_client(socket_file_path, logger).await?;

        Ok(Self {
            command: ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            ),
        })
    }
}

#[async_trait]
impl ImageClient for SimpleImageClient {
    async fn find_by_id(&mut self, id: &Id) -> Result<Image, DockerError> {
        self.command.find_by_id(id).await
    }

    async fn find_by_name(&mut self, image_name: &ImageName) -> Result<Image, DockerError> {
        self.command.find_by_name(image_name).await
    }

    async fn list(&mut self) -> Result<Vec<Image>, DockerError> {
        self.command.list().await
    }

    async fn pull(&mut self, image_name: &ImageName) -> Result<Image, DockerError> {
        self.command.pull(image_name).await
    }
}

#[async_trait]
pub trait ContainerClient {
    async fn find_by_id(&mut self, id: &str) -> Result<Container, DockerError>;
    async fn find_by_name(&mut self, name: &str) -> Result<Container, DockerError>;
    async fn create(
        &mut self,
        container_definition: ContainerDefinition,
    ) -> Result<Container, DockerError>;
    async fn start(&mut self, id: &str) -> Result<(), DockerError>;
}

#[derive(Debug, PartialEq, Clone)]
pub struct MountDefinition {
    pub name: String,
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub writable: bool,
}

impl Serialize for MountDefinition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("VolumeMount", 8)?;

        state.serialize_field("Type", "bind")?;
        state.serialize_field("Name", &self.name)?;
        state.serialize_field("Source", &path_to_string(&self.host_path))?;
        state.serialize_field("Target", &path_to_string(&self.container_path))?;
        state.serialize_field("RW", &self.writable)?;

        state.end()
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ImageIdentifier {
    Id(Id),
    ImageName(ImageName),
}

#[derive(Debug, PartialEq, Clone)]
pub struct ContainerUser {
    pub user_id: u32,
    pub group_id: u32,
}

impl Serialize for ContainerUser {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&format!("{}:{}", self.user_id, self.group_id))
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct ContainerDefinition {
    pub name: String,
    pub run_as: Option<ContainerUser>,
    pub command: Option<String>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub image_name: ImageIdentifier,
    pub mounts: Vec<MountDefinition>,
    pub added_capabilities: Vec<Capability>,
    pub labels: Vec<Label>,
}

pub struct SimpleContainerClient {
    container_command: ContainerCommand,
    image_command: ImageCommand,
}

impl SimpleContainerClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let rest_client = build_client(socket_file_path, logger).await?;
        let rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync>> =
            Arc::new(tokio::sync::Mutex::new(rest_client));

        let json_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>> =
            Arc::new(JsonParser::new());
        let chunked_json_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>> =
            Arc::new(ChunkedJsonParser::new());

        Ok(Self {
            image_command: ImageCommand::new(
                Arc::clone(&rest_client),
                Arc::clone(&json_parser),
                Arc::clone(&chunked_json_parser),
            ),
            container_command: ContainerCommand::new(rest_client, json_parser),
        })
    }

    async fn create_container(
        &mut self,
        inspected_container: InspectedContainer,
    ) -> Result<Container, DockerError> {
        let image = self
            .image_command
            .find_by_id(&inspected_container.image_id)
            .await?;

        let result = Container {
            id: inspected_container.id,
            name: inspected_container.name,
            image,
            status: inspected_container.state.status,
            mounts: inspected_container.mounts,
            environment_variables: inspected_container.config.env,
            labels: inspected_container.config.labels,
        };

        Ok(result)
    }
}

#[async_trait]
impl ContainerClient for SimpleContainerClient {
    async fn find_by_id(&mut self, id: &str) -> Result<Container, DockerError> {
        let inspected_container = self.container_command.find_by_id(id).await?;
        let result = self.create_container(inspected_container).await?;
        Ok(result)
    }

    async fn find_by_name(&mut self, name: &str) -> Result<Container, DockerError> {
        let inspected_container = self.container_command.find_by_name(name).await?;
        let result = self.create_container(inspected_container).await?;
        Ok(result)
    }

    async fn create(
        &mut self,
        container_definition: ContainerDefinition,
    ) -> Result<Container, DockerError> {
        let inspected_container = self.container_command.create(container_definition).await?;
        let result = self.create_container(inspected_container).await?;
        Ok(result)
    }

    async fn start(&mut self, id: &str) -> Result<(), DockerError> {
        self.container_command.start(id).await
    }
}

#[async_trait]
pub trait NetworkClient {
    async fn inspect_by_id(&mut self, id: &str) -> Result<Network, DockerError>;
    async fn inspect_by_name(&mut self, name: &str) -> Result<Network, DockerError>;
    async fn find_connected_containers_by_id(
        &mut self,
        network_id: &str,
    ) -> Result<Vec<Container>, DockerError>;
    async fn find_connected_containers_by_name(
        &mut self,
        network_name: &str,
    ) -> Result<Vec<Container>, DockerError>;
    async fn create(&mut self, name: &str, labels: Vec<Label>) -> Result<Network, DockerError>;
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

pub struct SimpleNetworkClient {
    network_command: NetworkCommand,
    container_client: SimpleContainerClient,
}

impl SimpleNetworkClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let rest_client = build_client(socket_file_path, logger).await?;
        let rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync>> =
            Arc::new(tokio::sync::Mutex::new(rest_client));

        let json_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>> =
            Arc::new(JsonParser::new());
        let chunked_json_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>> =
            Arc::new(ChunkedJsonParser::new());

        Ok(Self {
            container_client: SimpleContainerClient {
                container_command: ContainerCommand::new(
                    Arc::clone(&rest_client),
                    Arc::clone(&json_parser),
                ),
                image_command: ImageCommand::new(
                    Arc::clone(&rest_client),
                    Arc::clone(&json_parser),
                    Arc::clone(&chunked_json_parser),
                ),
            },
            network_command: NetworkCommand::new(rest_client, json_parser),
        })
    }

    async fn load_containers(&mut self, container_ids: Vec<String>) -> Vec<Container> {
        let mut result = Vec::with_capacity(container_ids.len());

        for id in container_ids {
            if let Ok(container) = self.container_client.find_by_id(&id).await {
                result.push(container);
            }
        }

        result
    }
}

#[async_trait]
impl NetworkClient for SimpleNetworkClient {
    async fn inspect_by_id(&mut self, id: &str) -> Result<Network, DockerError> {
        self.network_command.inspect_by_id(id).await
    }

    async fn inspect_by_name(&mut self, name: &str) -> Result<Network, DockerError> {
        self.network_command.inspect_by_name(name).await
    }

    async fn find_connected_containers_by_id(
        &mut self,
        network_id: &str,
    ) -> Result<Vec<Container>, DockerError> {
        let container_ids = self
            .network_command
            .find_connected_containers_by_id(network_id)
            .await?;

        Ok(self.load_containers(container_ids).await)
    }
    async fn find_connected_containers_by_name(
        &mut self,
        network_name: &str,
    ) -> Result<Vec<Container>, DockerError> {
        let container_ids = self
            .network_command
            .find_connected_containers_by_name(network_name)
            .await?;

        Ok(self.load_containers(container_ids).await)
    }
    async fn create(&mut self, name: &str, labels: Vec<Label>) -> Result<Network, DockerError> {
        self.network_command.create(name, labels).await
    }

    async fn connect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        self.network_command.connect(network, container).await
    }

    async fn disconnect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        self.network_command.disconnect(network, container).await
    }
}

#[cfg(test)]
mod tests {
    mod id_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_id")]
            id: Id,
        }

        #[test]
        fn should_deserialize_ids() {
            let json = r#"
                {
                  "id": "alg:123456"
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string()
                },
                wrapper.id
            );
        }

        #[test]
        fn should_deserialize_ids_without_alg() {
            let json = r#"
                {
                  "id": "123456"
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                Id {
                    algorithm: "missing-algorithim".to_string(),
                    hex: "123456".to_string()
                },
                wrapper.id
            );
        }
    }

    mod labels_deserializer {
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

    mod labels_serializer {
        use super::super::*;

        use serde_json::Value;
        use serde_json::json;

        #[derive(Debug, Serialize)]
        struct Wrapper {
            #[serde(serialize_with = "serialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_serialize_labels() {
            let wrapper = Wrapper {
                labels: vec![Label::new("foo", "bar"), Label::new("baz", "qux")],
            };

            let json_str = serde_json::to_string(&wrapper).unwrap();
            let expected = json!({
                "labels": {
                    "foo": "bar",
                    "baz": "qux"
                }
            });

            let actual: Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(expected, actual);
        }
    }
}
