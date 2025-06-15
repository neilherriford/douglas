pub mod container;
pub mod file_system;
pub mod image;
pub mod network;

use file_system::FileSystem;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::log::Logger;
use simple_rest_client::unix_domain_socket::{BuilderError, build_client};
use simple_rest_client::{Parser, Request, Response, RestClient, RestClientError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Id {
    pub algorithm: String,
    pub hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Label {
    pub name: String,
    pub value: String,
}

impl Label {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

pub(crate) fn deserialize_labels<'de, D>(deserializer: D) -> Result<Vec<Label>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Lables to be an object"))?;

    let result = obj
        .iter()
        .map(|(name, value)| {
            if let Some(value) = value.as_str() {
                Ok(Label {
                    name: name.as_str().to_string(),
                    value: value.to_string(),
                })
            } else {
                Err(serde::de::Error::custom("Expected value to be a string"))
            }
        })
        .collect::<Result<_, D::Error>>()?;

    Ok(result)
}

pub(crate) fn serialize_labels<S>(labels: &Vec<Label>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(labels.len()))?;
    for label in labels {
        map.serialize_entry(&label.name, &label.value)?;
    }
    map.end()
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
    #[error("Insufficnet number of chunks in response")]
    InsufficientChunksError,

    #[error("Excessive number of chunks in response")]
    ExcessiveChunksError,

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

    #[error("Not found")]
    NotFoundError,

    #[error("Invalid argument, '{name}: {given}' {message}")]
    InvalidArgumentError {
        name: String,
        given: String,
        message: String,
    },

    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Invalid path")]
    PathError { path: PathBuf, message: String },
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
    mount_root: PathBuf,
    fs: Box<dyn FileSystem + Send>,
}

impl SimpleDockerClient {
    pub async fn build(
        socket_file: &'static Path,
        mount_root: &Path,
        file_system: impl FileSystem + Send + 'static,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, DockerError> {
        let client = build_client(socket_file, logger, ChunkedJsonParser::new()).await?;

        let mount_root = file_system.canonicalize(mount_root)?;

        if mount_root.is_file() {
            return Err(DockerError::PathError {
                path: mount_root,
                message: "Expected directory".to_string(),
            });
        }

        Ok(Self {
            rest_client: Box::new(client),
            mount_root,
            fs: Box::new(file_system),
        })
    }

    fn expect_non_empty_string_argument(
        &self,
        argument_name: &str,
        argument: &String,
    ) -> Result<(), DockerError> {
        if argument.len() == 0 {
            Err(DockerError::InvalidArgumentError {
                name: argument_name.to_string(),
                given: argument.to_string(),
                message: "Cannot be blank".to_string(),
            })
        } else {
            Ok(())
        }
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

    fn expect_okay(&self, response: Response<Vec<Json>>) -> Result<Vec<Json>, DockerError> {
        match response {
            Response::Okay { body, .. } => Ok(if let Some(chunks) = body {
                chunks
            } else {
                vec![]
            }),
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
            Response::Error { status: 404, .. } => Err(DockerError::NotFoundError),
            Response::Error { status, body, .. } => Err(DockerError::UnexpectedResponseError {
                status,
                body,
                message: "non successful response".to_string(),
            }),
        }
    }

    fn expect_created(&self, response: Response<Vec<Json>>) -> Result<Vec<Json>, DockerError> {
        match response {
            Response::Okay { headers: _, body } => Err(DockerError::UnexpectedResponseError {
                status: 200,
                body,
                message: "expected CREATED, but recieved OK".to_string(),
            }),
            Response::Created { body, .. } => Ok(if let Some(chunks) = body {
                chunks
            } else {
                vec![]
            }),
            Response::NoContent { .. } => Err(DockerError::UnexpectedResponseError {
                status: 204,
                body: None,
                message: "expected CREATED, but recieved NO CONTENT".to_string(),
            }),
            Response::Error { status: 404, .. } => Err(DockerError::NotFoundError),
            Response::Error { status, body, .. } => Err(DockerError::UnexpectedResponseError {
                status,
                body,
                message: "non successful response".to_string(),
            }),
        }
    }

    fn expect_single_chunk<T>(&mut self, body: Vec<Json>) -> Result<T, DockerError>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut chunks = body.into_iter();

        match (chunks.next(), chunks.next()) {
            (None, _) => Err(DockerError::InsufficientChunksError),
            (Some(json), None) => Ok(from_value::<T>(json)?),
            (Some(_), Some(_)) => Err(DockerError::ExcessiveChunksError),
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

    mod labels_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_err_if_invalid_format() {
            let json = r#"
                {
                    "labels": 4
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_err_if_invalid_data() {
            let json = r#"
                {
                    "labels": {"magic": 42}
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_deserialize_labels() {
            let json = r#"
                {
                    "labels": {
                        "foo": "bar",
                        "baz": "qux"
                    }
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();

            assert_eq!(
                vec![
                    Label {
                        name: "baz".to_string(),
                        value: "qux".to_string()
                    },
                    Label {
                        name: "foo".to_string(),
                        value: "bar".to_string()
                    },
                ],
                wrapper.labels
            );
        }
    }

    mod labels_serializer {
        use super::super::*;

        use serde_json::Value;
        use serde_json::json;

        #[derive(Debug, Serialize)]
        struct Wrapper {
            #[serde(serialize_with = "serialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_serialize_labels() {
            let wrapper = Wrapper {
                labels: vec![Label::new("foo", "bar"), Label::new("baz", "qux")],
            };

            let json_str = serde_json::to_string(&wrapper).unwrap();
            let expected = json!({
                "labels": {
                    "foo": "bar",
                    "baz": "qux"
                }
            });

            let actual: Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(expected, actual);
        }
    }
}
