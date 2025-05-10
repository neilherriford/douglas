pub mod image;

use serde_json::value::Value as Json;
use simple_rest_client::log::Logger;
use simple_rest_client::unix_domain_socket::{BuilderError, build_client};
use simple_rest_client::{Parser, Response, RestClient, RestClientError};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChunkedJsonParserError {
    #[error("HTTP client error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
pub struct ChunkedJsonParser {}

impl ChunkedJsonParser {
    pub fn new() -> Self {
        Self {}
    }
}

impl Parser<String, Vec<Json>> for ChunkedJsonParser {
    type ParseError = ChunkedJsonParserError;

    fn parse(&self, input: String) -> Result<Vec<Json>, Self::ParseError> {
        input
            .split("\r\n")
            .filter(|chunk| chunk.len() > 0)
            .map(|chunk| serde_json::from_str(chunk).map_err(|e| e.into()))
            .collect()
    }
}

#[derive(Error, Debug)]
pub enum DockerError {
    #[error("Missing response body")]
    MissingBodyError,

    #[error("Received unexpected response with status: {status}, {message}")]
    UnexpectedResponseError {
        status: u16,
        body: Option<Vec<Json>>,
        message: String,
    },

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Init error: {0}")]
    InitError(#[from] BuilderError),

    #[error("Ambiguous match")]
    AmbiguousMatchError,

    #[error("Ambiguous match")]
    ParseError(#[from] serde_json::Error),

    #[error("API error {0}")]
    ApiError(String),
}

pub struct SimpleDockerClient {
    rest_client: Box<dyn RestClient<Vec<Json>> + Send>,
}

impl SimpleDockerClient {
    pub async fn build(
        socket_file_path: String,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let client = build_client(socket_file_path, logger, ChunkedJsonParser::new()).await?;

        Ok(Self {
            rest_client: Box::new(client),
        })
    }

    fn expect_no_docker_errors(&self, responses: Vec<Json>) -> Result<(), DockerError> {
        for response in responses {
            if let Some(message) = response.get("error") {
                let msg = match message.as_str() {
                    Some(text) => text.to_string(),
                    None => message.to_string(),
                };

                return Err(DockerError::ApiError(msg));
            }
        }

        Ok(())
    }

    fn expect_ok_with_body(&self, response: Response<Vec<Json>>) -> Result<Vec<Json>, DockerError> {
        match response {
            Response::Okay {
                headers: _,
                body: Some(chunks),
            } => Ok(chunks),
            Response::Okay {
                headers: _,
                body: None,
            } => Err(DockerError::MissingBodyError),
            Response::Created { body, .. } => Err(DockerError::UnexpectedResponseError {
                status: 201,
                body,
                message: "expected OK, but recieved CREATED".to_string(),
            }),
            Response::NoContent { .. } => Err(DockerError::UnexpectedResponseError {
                status: 204,
                body: None,
                message: "expected OK, but recieved NO CONTENT".to_string(),
            }),
            Response::Error { status, body, .. } => Err(DockerError::UnexpectedResponseError {
                status,
                body,
                message: "non successful response".to_string(),
            }),
        }
    }
}
