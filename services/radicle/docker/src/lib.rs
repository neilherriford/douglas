use serde::{Deserialize, Deserializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::log::Logger;
use simple_rest_client::unix_domain_socket::build_client;
use simple_rest_client::{Parser, Request, Response, RestClient};
use std::error::Error;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum DockerError {
    #[error("Unexpected response: {0}")]
    UnexpectedResponse(String),

    #[error("Received error status: {status}")]
    ErrorResponse { status: u16, body: Option<Json> },

    #[error("Not implemented yet.")]
    NotImplemented,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Image {
    #[serde(rename = "Id")]
    pub id: String,

    #[serde(rename = "RepoTags")]
    #[serde(deserialize_with = "deserialize_tags")]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Tag {
    pub name: String,
    pub version: String,
}

fn deserialize_tags<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_strings: Vec<String> = Deserialize::deserialize(deserializer)?;

    let tags = raw_strings
        .into_iter()
        .map(|raw_tag| {
            let parts: Vec<&str> = raw_tag.split(':').collect();
            let (name, version) = match parts.as_slice() {
                [first, second] => (first.to_string(), second.to_string()),
                _ => (raw_tag, String::from("")),
            };
            Tag { name, version }
        })
        .collect();

    Ok(tags)
}

#[async_trait::async_trait]
pub trait DockerClient {
    async fn list_images(&mut self) -> Result<Vec<Image>, Box<dyn Error>>;
}

pub struct SimpleDockerClient {
    rest_client: Box<dyn RestClient<Json> + Send>,
}

#[async_trait::async_trait]
impl DockerClient for SimpleDockerClient {
    async fn list_images(&mut self) -> Result<Vec<Image>, Box<(dyn Error)>> {
        let req = Request::Get {
            path: "/images/json".to_string(),
            headers: None,
        };

        let response: Response<Json> = self.rest_client.execute(&req).await?;

        match response {
            Response::Okay {
                headers: _,
                body: Some(body),
            } => Ok(from_value::<Vec<Image>>(body)?),
            Response::Okay { body: None, .. } => Err(Box::new(DockerError::UnexpectedResponse(
                "Expected non-empty body".to_string(),
            ))),
            Response::Created { .. } => Err(Box::new(DockerError::UnexpectedResponse(
                "Expected OK, but got Created status".to_string(),
            ))),
            Response::NoContent { .. } => Err(Box::new(DockerError::UnexpectedResponse(
                "Expected OK, but got No Content status".to_string(),
            ))),
            Response::Error {
                headers: _,
                status,
                body,
            } => Err(Box::new(DockerError::ErrorResponse {
                status: status,
                body: body,
            })),
        }
    }
}

#[derive(Debug)]
struct JsonParser {}

impl JsonParser {
    pub fn new() -> Self {
        Self {}
    }
}

impl Parser<String, Json> for JsonParser {
    fn parse(&self, input: String) -> Result<Json, Box<(dyn Error)>> {
        serde_json::from_str(&input).map_err(|e| e.into())
    }
}

impl SimpleDockerClient {
    pub async fn build(
        socket_file_path: String,
        logger: Arc<dyn Logger>,
    ) -> Result<SimpleDockerClient, Box<dyn Error>> {
        let client = build_client(socket_file_path, logger, JsonParser::new()).await?;

        Ok(SimpleDockerClient {
            rest_client: Box::new(client),
        })
    }
}

#[cfg(test)]
mod tests {
    mod list_images {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_error_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: None,
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;

            match result {
                Err(error) => assert_eq!(
                    "Unexpected response: Expected non-empty body",
                    error.to_string()
                ),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_error_on_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Created {
                    headers: vec![],
                    body: None,
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;

            match result {
                Err(error) => assert_eq!(
                    "Unexpected response: Expected OK, but got Created status",
                    error.to_string()
                ),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_error_on_created_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_execute()
                .returning(|_req| Ok(Response::<Json>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;

            match result {
                Err(error) => assert_eq!(
                    "Unexpected response: Expected OK, but got No Content status",
                    error.to_string()
                ),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_error_on_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Error {
                    headers: vec![],
                    status: 500,
                    body: None,
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;

            match result {
                Err(error) => assert_eq!("Received error status: 500", error.to_string()),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_return_error_if_the_json_is_unexpected() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!({"unexpected": "json format"})),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;
            assert_eq!(true, result.is_err());
        }

        #[tokio::test]
        async fn should_list_images() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!(
[
  {
    "Containers": -1,
    "Created": 1732220204,
    "Id": "sha256:49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28",
    "Labels": null,
    "ParentId": "",
    "RepoDigests":
    [
      "example@sha256:888402a8cd6075c5dc83a31f58287f13306c318eaad016661ed12e076f3e6341"
    ],
    "RepoTags":
    [
      "foo:bar"
    ],
    "SharedSize": -1,
    "Size": 12345,
    "VirtualSize": 67890
  }
]
)),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list_images().await;

            match result {
                Ok(actual) => {
                    let expected = vec![Image {
                        id: "sha256:49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28".to_string(),
                        tags: vec![Tag {name: "foo".to_string(), version: "bar".to_string()}]
                    }];

                    assert_eq!(expected, actual)
                }
                _ => {
                    unreachable!("Expected a vec of images!")
                }
            }
        }
    }
}
