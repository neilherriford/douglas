mod commands;
use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use log::Logger;
use serde::Deserialize;
use simple_rest_client::{
    RestClient, RestClientError,
    parsers::json::{JsonParser, JsonParserError},
    unix_domain_socket::{BuilderError, build_client},
};
use thiserror::Error;

use crate::commands::status::StatusCommand;

#[derive(Error, Debug)]
pub enum OpenBaoError {
    #[error("Received unexpected response with status: {status}, {message}")]
    UnexpectedResponse {
        status: u16,
        body: Option<String>,
        message: String,
    },

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("Init error: {0}")]
    InitError(#[from] BuilderError),
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

#[async_trait]
pub trait OpenBaoClient {
    async fn status(&mut self) -> Result<Status, OpenBaoError>;
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
}
