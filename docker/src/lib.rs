mod client;
mod commands;
pub use client::{Ping, UdsPing};
use file_system::{FileSystemError, path_to_string};
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::Value as Json;
use simple_rest_client::assertions::AssertionError;
use simple_rest_client::unix_domain_socket::BuilderError;
use simple_rest_client::{Request, RestClientError};
use std::collections::HashSet;
use std::path::PathBuf;
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
    #[error("Ping failed: {0}")]
    PingFailed(String),

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Client response error: {status}, {message}")]
    ResponseError {
        status: u16,
        body: Option<String>,
        message: String,
    },

    #[error("Client response conntent error: {0}")]
    ClientResponseContentError(String),

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

    #[error("Resource not found")]
    ResourceNotFound,
}

impl From<AssertionError> for DockerError {
    fn from(value: AssertionError) -> Self {
        match value {
            AssertionError::UnexpectedResponseError {
                status,
                body,
                message,
            } => DockerError::ResponseError {
                status,
                body,
                message,
            },
            AssertionError::MissingBody => {
                DockerError::ClientResponseContentError("Missing body".into())
            }
            AssertionError::NotFoundError => DockerError::ResourceNotFound,
        }
    }
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

impl std::str::FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Versions cannot be empty".to_string());
        }

        if s == "latest" {
            Ok(Version::Latest)
        } else {
            Ok(Version::Specific(s.to_string()))
        }
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
