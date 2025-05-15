use crate::deserialize_id;
use crate::{DockerError, Id, SimpleDockerClient};
use serde::{Deserialize, Deserializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::{Request, Response};
use std::collections::HashMap;

#[derive(Debug)]
pub enum Version {
    Latest,
    Specific(String),
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let formatted = match self {
            Version::Latest => "latest".to_string(),
            Version::Specific(version) => version.to_string(),
        };

        write!(f, "{}", formatted)
    }
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
pub trait Repository {
    async fn list(&mut self) -> Result<Vec<Image>, DockerError>;
    async fn find(&mut self, id: &Id) -> Result<Option<Image>, DockerError>;
    async fn where_named(&mut self, name: &str) -> Result<Option<Vec<Image>>, DockerError>;
    async fn pull(&mut self, name: &str, version: Version) -> Result<Image, DockerError>;
    async fn inspect_by_name(&mut self, name: &str, version: Version)
    -> Result<Image, DockerError>;
    async fn inspect_by_id(&mut self, id: Id) -> Result<Image, DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn list(&mut self) -> Result<Vec<Image>, DockerError> {
        let req = Request::Get {
            path: "/images/json".to_string(),
            headers: None,
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;
        let chunks = self.expect_ok_with_body(response)?;
        chunks
            .into_iter()
            .map(from_value::<Vec<Image>>)
            .collect::<Result<Vec<Vec<Image>>, _>>()
            .map(|vecs| vecs.into_iter().flatten().collect())
            .map_err(Into::into)
    }

    async fn find(&mut self, id: &Id) -> Result<Option<Image>, DockerError> {
        let mut matches = self
            .list()
            .await?
            .into_iter()
            .filter(|image| image.id == *id);

        match (matches.next(), matches.next()) {
            (Some(first), None) => Ok(Some(first)),
            (None, _) => Ok(None),
            _ => Err(DockerError::AmbiguousMatchError),
        }
    }

    async fn where_named(&mut self, name: &str) -> Result<Option<Vec<Image>>, DockerError> {
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

    async fn pull(&mut self, name: &str, version: Version) -> Result<Image, DockerError> {
        let req = Request::Post {
            path: simple_rest_client::create_path_and_query_string(
                "/images/create",
                HashMap::from([("fromImage", name), ("tag", version.to_string().as_str())]),
            ),
            body: None,
            headers: None,
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;
        let chunks = self.expect_ok_with_body(response)?;
        self.expect_no_docker_errors(chunks)?;

        Ok(self.inspect_by_name(name, version).await?)
    }

    async fn inspect_by_name(
        &mut self,
        name: &str,
        version: Version,
    ) -> Result<Image, DockerError> {
        let request = Request::Get {
            path: format!("/images/{}:{}/json", name, version),
            headers: None,
        };

        return self.inspect(request).await;
    }

    async fn inspect_by_id(&mut self, id: Id) -> Result<Image, DockerError> {
        let request = Request::Get {
            path: format!("/images/{}/json", id),
            headers: None,
        };

        return self.inspect(request).await;
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: None,
                })
            });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_error_on_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Vec<Json>>::Created {
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
                    "Received unexpected response with status: 201, expected OK, but recieved CREATED",
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
                .returning(|_req| Ok(Response::<Vec<Json>>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            match result {
                Err(error) => assert_eq!(
                    "Received unexpected response with status: 204, expected OK, but recieved NO CONTENT",
                    error.to_string()
                ),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_error_on_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Vec<Json>>::Error {
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
                Err(error) => assert_eq!(
                    "Received unexpected response with status: 500, non successful response",
                    error.to_string()
                ),
                _ => unreachable!("Expected an error!"),
            }
        }

        #[tokio::test]
        async fn should_return_error_if_the_json_is_unexpected() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_execute().returning(|_req| {
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!({"unexpected": "json format"})]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!(
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
)]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!(
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
)]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!(
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
)]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([
                        {"Id": "alg:123", "RepoTags": ["foo:bar"]},
                        {"Id": "alg:123", "RepoTags": ["bas:qux"]}
                    ])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![
                        json!([{"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]}]),
                    ]),
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
                Ok(Response::<Vec<Json>>::Okay {
                    headers: vec![],
                    body: Some(vec![json!([
                        {"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]},
                        {"Id": "alg:456", "RepoTags": ["name:foo"]},
                    ])]),
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

    mod pull {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn shoud_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn shoud_error_if_received_got_created() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Created {
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 201,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_got_no_content() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| Ok(Response::<Vec<Json>>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 204,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_error() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Error {
                        status: 500,
                        body: None,
                        headers: vec![],
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 500,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn should_error_if_docker_error() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({"error":"Oops all errors"})]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            match result {
                Err(DockerError::ApiError(msg)) => assert_eq!(msg, "Oops all errors"),
                _ => panic!("expected DockerError::ApiError"),
            }
        }

        #[tokio::test]
        async fn should_pull_latest() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=latest")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![
                            json!({"status":"Pulling from library/foo","id":"latest"}),
                        ]),
                    })
                });

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:123456",
                          "RepoTags":["foo:latest"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            match result {
                Ok(image) => {
                    assert_eq!(
                        Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "123456".to_string()
                            },
                            tags: vec! {Tag{name: "foo".to_string(), version: "latest".to_string()}}
                        },
                        image
                    );
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_pull_specific_versio() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Post { path, .. } = req {
                        path.starts_with("/images/create?")
                            && path.contains("tag=1.2.3")
                            && path.contains("fromImage=foo")
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![
                            json!({"status":"Pulling from library/foo","id":"latest"}),
                        ]),
                    })
                });

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:1.2.3/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:123456",
                          "RepoTags":["foo:1.2.3"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .pull("foo", Version::Specific("1.2.3".to_string()))
                .await;

            match result {
                Ok(image) => {
                    assert_eq!(
                        Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "123456".to_string()
                            },
                            tags: vec! {Tag{name: "foo".to_string(), version: "1.2.3".to_string()}}
                        },
                        image
                    );
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }
    }

    mod inspect_by_name {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Created {
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 201,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| Ok(Response::<Vec<Json>>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 204,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Error {
                        status: 500,
                        headers: vec![],
                        body: None,
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 500,
                    body: None,
                    ..
                })
            ));
        }

        #[tokio::test]
        async fn should_error_if_too_little_json() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status, message, ..
                }) => {
                    assert_eq!(200, status);
                    assert_eq!("no results", message);
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_error_if_too_much_json() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![
                            json!({
                            "Id": "alg:123456",
                              "RepoTags":["foo:latest"],
                            }),
                            json!({
                            "Id": "alg:987654",
                              "RepoTags":["foo:1.2.3"],
                            }),
                        ]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status, message, ..
                }) => {
                    assert_eq!(200, status);
                    assert_eq!("too many results", message);
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_inspect_latest() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:latest/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:123456",
                          "RepoTags":["foo:latest"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;

            match result {
                Ok(image) => {
                    assert_eq!(
                        Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "123456".to_string()
                            },
                            tags: vec! {Tag{name: "foo".to_string(), version: "latest".to_string()}}
                        },
                        image
                    );
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_inspect_specific() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/foo:1.2.3/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:123456",
                          "RepoTags":["foo:1.2.3"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_name("foo", Version::Specific("1.2.3".to_string()))
                .await;

            match result {
                Ok(image) => {
                    assert_eq!(
                        Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "123456".to_string()
                            },
                            tags: vec! {Tag{name: "foo".to_string(), version: "1.2.3".to_string()}}
                        },
                        image
                    );
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }
    }

    mod inspect_by_id {
        use super::super::*;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_inspect_by_id() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/images/alg:123456/json"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                        "Id": "alg:123456",
                          "RepoTags":["foo:1.2.3"],
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            match result {
                Ok(image) => {
                    assert_eq!(
                        Image {
                            id: Id {
                                algorithm: "alg".to_string(),
                                hex: "123456".to_string()
                            },
                            tags: vec! {Tag{name: "foo".to_string(), version: "1.2.3".to_string()}}
                        },
                        image
                    );
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }
    }
}
