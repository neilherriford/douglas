use crate::{DockerError, Parser};
use serde_json::value::Value as Json;
use thiserror::Error;

#[derive(Debug, Default)]
pub struct ChunkedJsonParser {}

impl ChunkedJsonParser {
    pub fn new() -> Self {
        Self {}
    }
}

impl Parser<Vec<Json>> for ChunkedJsonParser {
    type ParseError = JsonParserError;

    fn parse(&self, input: String) -> Result<Vec<Json>, Self::ParseError> {
        input
            .split("\r\n")
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| serde_json::from_str(chunk).map_err(|e| e.into()))
            .collect()
    }
}

impl From<serde_json::Error> for JsonParserError {
    fn from(value: serde_json::Error) -> Self {
        JsonParserError::Error {
            line: value.line(),
            column: value.column(),
            message: value.to_string(),
        }
    }
}

#[derive(Error, Debug)]
pub enum JsonParserError {
    #[error("JSON parse error: {line}:{column} {message}")]
    Error {
        line: usize,
        column: usize,
        message: String,
    },
}

#[derive(Debug, Default)]
pub struct JsonParser {}

impl JsonParser {
    pub fn new() -> Self {
        Self {}
    }
}

impl Parser<Json> for JsonParser {
    type ParseError = JsonParserError;

    fn parse(&self, input: String) -> Result<Json, Self::ParseError> {
        match serde_json::from_str(&input) {
            Ok(json) => Ok(json),
            Err(err) => Err(JsonParserError::Error {
                line: err.line(),
                column: err.column(),
                message: err.to_string(),
            }),
        }
    }
}

impl From<JsonParserError> for DockerError {
    fn from(value: JsonParserError) -> Self {
        match value {
            JsonParserError::Error {
                line,
                column,
                message,
            } => DockerError::ParseError {
                line,
                column,
                message,
            },
        }
    }
}
