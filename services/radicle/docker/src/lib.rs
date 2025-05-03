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

    #[error("Ambiguous match")]
    AmbiguousMatch,

    #[error("Not implemented yet.")]
    NotImplemented,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Image {
    #[serde(rename = "Id")]
    #[serde(deserialize_with = "deserialize_id")]
    pub id: Id,

    #[serde(rename = "RepoTags")]
    #[serde(deserialize_with = "deserialize_tags")]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Id {
    pub algorithm: String,
    pub hex: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
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

fn deserialize_id<'de, D>(deserializer: D) -> Result<Id, D::Error>
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

#[async_trait::async_trait]
pub trait DockerImageRepository {
    async fn list(&mut self) -> Result<Vec<Image>, Box<dyn Error>>;
    async fn find(&mut self, id: &Id) -> Result<Option<Image>, Box<dyn Error>>;
    async fn where_named(&mut self, name: &str) -> Result<Option<Vec<Image>>, Box<dyn Error>>;
}

pub struct SimpleDockerClient {
    rest_client: Box<dyn RestClient<Json> + Send>,
}

#[async_trait::async_trait]
impl DockerImageRepository for SimpleDockerClient {
    async fn list(&mut self) -> Result<Vec<Image>, Box<(dyn Error)>> {
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

    async fn find(&mut self, id: &Id) -> Result<Option<Image>, Box<dyn Error>> {
        let mut matches = self
            .list()
            .await?
            .into_iter()
            .filter(|image| image.id == *id);

        match (matches.next(), matches.next()) {
            (Some(first), None) => Ok(Some(first)),
            (None, _) => Ok(None),
            _ => Err(Box::new(DockerError::AmbiguousMatch)),
        }
    }

    async fn where_named(&mut self, name: &str) -> Result<Option<Vec<Image>>, Box<dyn Error>> {
        let matches: Vec<_> = self
            .list()
            .await?
            .into_iter()
            .filter(|image| {
                image
                    .tags
                    .iter()
                    .any(|tag| tag.name == "name" && tag.version == name)
            })
            .collect();

        match matches.len() {
            0 => Ok(None),
            _ => Ok(Some(matches)),
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
    mod list {
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

            let result = client.list().await;

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

            let result = client.list().await;

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

            let result = client.list().await;

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

            let result = client.list().await;

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

            let result = client.list().await;
            assert_eq!(true, result.is_err());
        }

        #[tokio::test]
        async fn should_list() {
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

            let result = client.list().await;

            match result {
                Ok(actual) => {
                    let expected = vec![Image {
                        id: Id {
                            algorithm: "sha256".to_string(),
                            hex: "49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28"
                                .to_string(),
                        },
                        tags: vec![Tag {
                            name: "foo".to_string(),
                            version: "bar".to_string(),
                        }],
                    }];

                    assert_eq!(expected, actual)
                }
                _ => {
                    unreachable!("Expected a vec of images!")
                }
            }
        }

        #[tokio::test]
        async fn should_decode_tags_without_semicolons() {
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
      "foo"
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

            let result = client.list().await;

            match result {
                Ok(actual) => {
                    let expected = vec![Image {
                        id: Id {
                            algorithm: "sha256".to_string(),
                            hex: "49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28"
                                .to_string(),
                        },
                        tags: vec![Tag {
                            name: "foo".to_string(),
                            version: "".to_string(),
                        }],
                    }];

                    assert_eq!(expected, actual)
                }
                _ => {
                    unreachable!("Expected a vec of images!")
                }
            }
        }

        #[tokio::test]
        async fn should_list_with_simple_ids() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!(
[
  {
    "Containers": -1,
    "Created": 1732220204,
    "Id": "no-alg-just-value",
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

            let result = client.list().await;

            match result {
                Ok(actual) => {
                    let expected = vec![Image {
                        id: Id {
                            algorithm: "missing-algorithim".to_string(),
                            hex: "no-alg-just-value".to_string(),
                        },
                        tags: vec![Tag {
                            name: "foo".to_string(),
                            version: "bar".to_string(),
                        }],
                    }];

                    assert_eq!(expected, actual)
                }
                _ => {
                    unreachable!("Expected a vec of images!")
                }
            }
        }
    }

    mod find {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_find_none() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find(&Id {
                    algorithm: "alg".to_string(),
                    hex: "456".to_string(),
                })
                .await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_none());
        }

        #[tokio::test]
        async fn should_find_one() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string(),
                })
                .await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_some());
            let image = found.unwrap();
            assert_eq!("alg".to_string(), image.id.algorithm);
            assert_eq!("123".to_string(), image.id.hex);
            assert_eq!(1, image.tags.len());
            let tag = image.tags.first().unwrap();
            assert_eq!("foo".to_string(), tag.name);
            assert_eq!("bar".to_string(), tag.version);
        }

        #[tokio::test]
        async fn should_error_if_more_than_one_match() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([
                        {"Id": "alg:123", "RepoTags": ["foo:bar"]},
                        {"Id": "alg:123", "RepoTags": ["bas:qux"]}
                    ])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .find(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string(),
                })
                .await;

            assert_eq!(true, result.is_err());
            assert_eq!(
                "Ambiguous match".to_string(),
                result.err().unwrap().to_string()
            );
        }
    }

    mod where_named {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_find_none_when_no_name_tags() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("bar").await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_none());
        }

        #[tokio::test]
        async fn should_find_none_when_no_matches() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("bar").await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_none());
        }

        #[tokio::test]
        async fn should_find_one_match() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_some());
            let images = found.unwrap();
            assert_eq!(1, images.len());
            assert_eq!(
                Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string()
                },
                images.first().unwrap().id
            );
        }

        #[tokio::test]
        async fn should_find_one_match_with_multiple_name_tags() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([{"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]}])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;

            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_some());
            let images = found.unwrap();
            assert_eq!(1, images.len());
            assert_eq!(
                Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string()
                },
                images.first().unwrap().id
            )
        }

        #[tokio::test]
        async fn should_find_multiple_matches() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Json>::Okay {
                    headers: vec![],
                    body: Some(json!([
                        {"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]},
                        {"Id": "alg:456", "RepoTags": ["name:foo"]},
                    ])),
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;
            assert_eq!(true, result.is_ok());
            let found = result.unwrap();
            assert_eq!(true, found.is_some());
            let images = found.unwrap();
            assert_eq!(2, images.len());
            assert_eq!(
                true,
                images.iter().any(|image| image.id
                    == Id {
                        algorithm: "alg".to_string(),
                        hex: "123".to_string()
                    })
            );
            assert_eq!(
                true,
                images.iter().any(|image| image.id
                    == Id {
                        algorithm: "alg".to_string(),
                        hex: "456".to_string()
                    })
            );
        }
    }
}
