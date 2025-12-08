use serde_json::value::Value as Json;
use thiserror::Error;

use crate::parsers::Parser;

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
