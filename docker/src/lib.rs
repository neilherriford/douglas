pub mod client;
mod commands;

use docker_types::{
    EnvironmentVariable, HealthStatus, Id, Label, Mount, Status, Tag,
    deserialize_environment_variables, deserialize_id, deserialize_labels, deserialize_tags,
};
use serde::{Deserialize, Deserializer};
use simple_rest_client::Request;
use simple_rest_client::assertions::AssertionError;
use std::collections::HashSet;
use thiserror::Error;

#[cfg(feature = "mock")]
pub use client::MockClient;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum DockerError {
    #[error("Ping failed: {0}")]
    FailedToCreateClient(String),

    #[error("Ping failed: {0}")]
    PingFailed(String),

    #[error("Resource not found")]
    ResourceNotFound,

    #[error("Invalid argument, '{name}: {given}' {message}")]
    InvalidArgumentError {
        name: String,
        given: String,
        message: String,
    },

    #[error("Ambiguous match")]
    AmbiguousMatchError,

    #[error("Invalid name '{name}': {description}")]
    InvalidName { name: String, description: String },

    #[error("General error {0}")]
    GeneralError(String),
}

fn to_general_error<TError>(error: TError) -> DockerError
where
    TError: std::fmt::Display,
{
    DockerError::GeneralError(error.to_string())
}

impl From<AssertionError> for DockerError {
    fn from(value: AssertionError) -> Self {
        if let AssertionError::NotFoundError = value {
            DockerError::ResourceNotFound
        } else {
            to_general_error(value)
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

    #[serde(rename = "StartedAt")]
    #[serde(deserialize_with = "deserialize_started_at")]
    pub started_at: time::OffsetDateTime,

    #[serde(rename = "Health")]
    pub health: Option<Health>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Health {
    #[serde(rename = "Status")]
    pub status: HealthStatus,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Config {
    #[serde(rename = "Cmd")]
    #[serde(deserialize_with = "deserialize_container_command")]
    pub command: Option<String>,

    #[serde(rename = "Env")]
    #[serde(deserialize_with = "deserialize_environment_variables")]
    pub env: Vec<EnvironmentVariable>,

    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

fn deserialize_started_at<'de, D>(deserializer: D) -> Result<time::OffsetDateTime, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    time::OffsetDateTime::parse(&value, &time::format_description::well_known::Rfc3339)
        .map_err(serde::de::Error::custom)
}

fn deserialize_container_command<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let tokens: Option<Vec<String>> = Option::deserialize(deserializer)?;
    tokens
        .map(|tokens| {
            shlex::try_join(tokens.iter().map(String::as_str)).map_err(serde::de::Error::custom)
        })
        .transpose()
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
