use crate::DockerError;
use serde_json::value::Value as Json;
use simple_rest_client::parsers::{Parser, json::JsonParserError};

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
