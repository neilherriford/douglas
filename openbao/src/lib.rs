mod commands;

use crate::commands::{
    init::{ConfigError, InitCommand},
    status::StatusCommand,
};
use async_trait::async_trait;
use log::Logger;
use serde::Deserialize;
use simple_rest_client::{
    RestClient, RestClientError,
    assertions::AssertionError,
    parsers::json::{JsonParser, JsonParserError},
    unix_domain_socket::{BuilderError, build_client},
};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum OpenBaoError {
    #[error("Already initialized")]
    AlreadyInitialized,

    #[error("Received unexpected response with status: {status}, {message}")]
    UnexpectedResponse {
        status: u16,
        body: Option<String>,
        message: String,
    },

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Client error: {0}")]
    ClientResponseError(#[from] AssertionError),

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("Init error: {0}")]
    InitError(#[from] BuilderError),

    #[error("Configuration error: {0}")]
    ConfigurationError(#[from] ConfigError),

    #[error("General Error")]
    Error(String),
}

impl From<serde_json::Error> for OpenBaoError {
    fn from(err: serde_json::Error) -> OpenBaoError {
        OpenBaoError::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

impl From<JsonParserError> for OpenBaoError {
    fn from(err: JsonParserError) -> OpenBaoError {
        match err {
            JsonParserError::Error {
                line,
                column,
                message,
            } => OpenBaoError::ParseError {
                line,
                column,
                message,
            },
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub enum ReplicationMode {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "primary")]
    Primary,
    #[serde(rename = "secondary")]
    Secondary,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Status {
    pub initialized: bool,
    pub sealed: bool,
    pub standby: bool,
    pub performance_standby: bool,
    pub replication_performance_mode: ReplicationMode,
    pub replication_dr_mode: ReplicationMode,
    pub server_time_utc: u32,
    pub version: String,
}

#[derive(Debug)]
pub struct Secrets {
    pub secrets: Vec<Secret>,
    pub root_token: String,
}

#[derive(Debug)]
pub struct Secret {
    pub key: String,
    pub base64: String,
}

#[async_trait]
pub trait OpenBaoClient {
    async fn status(&mut self) -> Result<Status, OpenBaoError>;
    async fn intialize(&mut self) -> Result<Secrets, OpenBaoError>;
}

pub struct SimpleOpenBaoClient {
    rest_client: Box<dyn RestClient>,
    parser: JsonParser,
}

impl SimpleOpenBaoClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, OpenBaoError> {
        let rest_client = build_client(socket_file_path, logger).await?;

        Ok(Self {
            rest_client: Box::new(rest_client),
            parser: JsonParser::new(),
        })
    }
}

#[async_trait]
impl OpenBaoClient for SimpleOpenBaoClient {
    async fn status(&mut self) -> Result<Status, OpenBaoError> {
        Ok(StatusCommand::new(self.rest_client.as_mut(), &self.parser)
            .perform()
            .await?)
    }

    async fn intialize(&mut self) -> Result<Secrets, OpenBaoError> {
        Ok(InitCommand::new(
            self.rest_client.as_mut(),
            &self.parser,
            commands::init::Config::new(10, 3)?,
        )
        .perform()
        .await?)
    }
}
