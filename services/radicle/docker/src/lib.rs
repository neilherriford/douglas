pub mod container;
pub mod image;

use serde::{Deserialize, Deserializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::log::Logger;
use simple_rest_client::unix_domain_socket::{BuilderError, build_client};
use simple_rest_client::{Parser, Request, Response, RestClient, RestClientError};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Id {
    pub algorithm: String,
    pub hex: String,
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

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("API error {0}")]
    ApiError(String),
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

    async fn expect_single_chunk<T>(&mut self, request: Request) -> Result<T, DockerError>
    where
        T: serde::de::DeserializeOwned,
    {
        let response: Response<Vec<Json>> = self.rest_client.execute(&request).await?;
        let mut chunks = self.expect_ok_with_body(response)?.into_iter();

        match (chunks.next(), chunks.next()) {
            (None, _) => Err(DockerError::UnexpectedResponseError {
                status: 200,
                body: None,
                message: "no results".to_string(),
            }),
            (Some(json), None) => Ok(from_value::<T>(json)?),
            (Some(first), Some(second)) => Err(DockerError::UnexpectedResponseError {
                status: 200,
                body: Some(vec![first, second]),
                message: "too many results".to_string(),
            }),
        }
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
}
