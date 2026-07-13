mod client;
mod commands;
pub use client::{Ping, UdsPing};
pub use docker_types::{
    Capability, ContainerDefinition, ContainerName, ContainerUser, DockerNameError,
    EnvironmentVariable, EnvironmentVariableName, Id, ImageIdentifier, Label, Mount,
    MountDefinition, MountName, MountType, NetworkName, Status, Tag, Version, VersionedImageName,
    VersionedImageNameParseError, deserialize_environment_variables, deserialize_id,
    deserialize_labels, serialize_capabilities, serialize_environment_variables,
    serialize_image_identifier, serialize_labels,
};
use file_system::FileSystemError;
use serde::{Deserialize, Deserializer};
use simple_rest_client::assertions::AssertionError;
use simple_rest_client::unix_domain_socket::BuilderError;
use simple_rest_client::{Request, RestClientError};
use std::collections::HashSet;
use std::path::PathBuf;
use thiserror::Error;

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
