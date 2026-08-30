mod client;
mod commands;

pub mod app_role;

pub use client::{Client, ClientFactory, SocketClient, SocketClientFactory};
#[cfg(feature = "mock")]
pub use client::{MockClient, MockClientFactory};

pub const SEEDLING_NAME: &str = "openbao";
pub const SOCKET_MOUNT_NAME: &str = "socket";
pub const SOCKET_NAME: &str = "openbao.sock";
pub const IMAGE_VERSION: &str = "2.6.2";
pub const AGENT_LOCAL_PROXY_PORT: u16 = 8100;
pub const API_PORT: u16 = 8201;

use simple_rest_client::{
    RestClientError, assertions::AssertionError, parsers::json::JsonParserError,
    unix_domain_socket::BuilderError,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Not authenticated")]
    NotAuthenticated,

    #[error("Unknown role: '(0)'")]
    UnknownRole(String),

    #[error("Already initialized")]
    AlreadyInitialized,

    #[error("Insufficient Secrets")]
    InsufficentSecrets,

    #[error("Unseal error: {0:?}")]
    UnsealError(Vec<String>),

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

    #[error("General Error")]
    Error(String),

    #[error("The threshold must be less than or equal to the number of shares")]
    InvalidThreshold,

    #[error("The threshold must non-zero")]
    ThresholdTooSmall,

    #[error("The shares must non-zero")]
    SharesTooSmall,
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Error {
        Error::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

impl From<JsonParserError> for Error {
    fn from(err: JsonParserError) -> Error {
        match err {
            JsonParserError::Error {
                line,
                column,
                message,
            } => Error::ParseError {
                line,
                column,
                message,
            },
        }
    }
}
