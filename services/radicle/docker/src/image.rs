use crate::deserialize_id;
use crate::{DockerError, Id, SimpleDockerClient};
use serde::{Deserialize, Deserializer};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::{Request, Response};
use std::collections::{HashMap, HashSet};

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
    pub tags: HashSet<Tag>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct Tag {
    pub name: String,
    pub version: String,
}

fn deserialize_tags<'de, D>(deserializer: D) -> Result<HashSet<Tag>, D::Error>
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
    async fn inspect_by_id(&mut self, id: &Id) -> Result<Image, DockerError>;
    async fn inspect_by_name(&mut self, name: &str, version: Version)
    -> Result<Image, DockerError>;
    async fn list(&mut self) -> Result<Vec<Image>, DockerError>;
    async fn pull(&mut self, name: &str, version: Version) -> Result<Image, DockerError>;
    async fn where_named(&mut self, name: &str) -> Result<Vec<Image>, DockerError>;
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

    async fn inspect_by_id(&mut self, id: &Id) -> Result<Image, DockerError> {
        let request = Request::Get {
            path: format!("/images/{}/json", id),
            headers: None,
        };

        return self.expect_single_chunk(request).await;
    }

    async fn where_named(&mut self, name: &str) -> Result<Vec<Image>, DockerError> {
        let result: Vec<_> = self
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
        Ok(result)
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

        return self.expect_single_chunk(request).await;
    }
}

#[cfg(test)]
mod tests {
    mod tag_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_tags")]
            tags: HashSet<Tag>,
        }

        #[test]
        fn should_error_on_non_array() {
            let json = r#"
                {
                  "tags": {"foo": "bar"}
                }
            "#;
            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_split_on_colons() {
            let json = r#"
                {
                  "tags": ["foo:bar", "baz:qux"]
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                vec![
                    Tag {
                        name: "foo".to_string(),
                        version: "bar".to_string()
                    },
                    Tag {
                        name: "baz".to_string(),
                        version: "qux".to_string()
                    },
                ]
                .into_iter()
                .collect::<HashSet<Tag>>(),
                wrapper.tags
            );
        }

        #[test]
        fn should_combine_if_missing_colon() {
            let json = r#"
                {
                  "tags": ["foo"]
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                vec![Tag {
                    name: "foo".to_string(),
                    version: String::new()
                }]
                .into_iter()
                .collect::<HashSet<Tag>>(),
                wrapper.tags
            );
        }
    }

    mod list {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_error_with_no_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/json", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_error_on_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/images/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_on_created_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/images/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_on_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/images/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_return_error_if_the_json_is_unexpected() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/json", Some(vec![json!("Oops")]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;
            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_list() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/json", Some(vec![json!(
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
            )]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.list().await;
            let expected = vec![Image {
                id: Id {
                    algorithm: "sha256".to_string(),
                    hex: "49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28"
                        .to_string(),
                },
                tags: vec![Tag {
                    name: "foo".to_string(),
                    version: "bar".to_string(),
                }]
                .into_iter()
                .collect(),
            }];

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }

    mod where_named {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_find_none_when_no_name_tags() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(vec![json!([{"Id": "alg:123", "RepoTags": ["foo:bar"]}])]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("bar").await;

            assert!(matches!(result, Ok(actual) if actual == vec![]));
        }

        #[tokio::test]
        async fn should_find_none_when_no_matches() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(vec![json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("bar").await;

            assert!(matches!(result, Ok(actual) if actual == vec![]));
        }

        #[tokio::test]
        async fn should_find_one_match() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(vec![json!([{"Id": "alg:123", "RepoTags": ["name:foo"]}])]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;

            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string(),
                },
                tags: vec![Tag {
                    name: "name".to_string(),
                    version: "foo".to_string(),
                }]
                .into_iter()
                .collect(),
            };

            assert!(matches!(result, Ok(actual) if actual == vec![expected]));
        }

        #[tokio::test]
        async fn should_find_one_match_with_multiple_name_tags() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(vec![json!(
                    [{"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]}]
                )]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;
            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123".to_string(),
                },
                tags: vec![
                    Tag {
                        name: "name".to_string(),
                        version: "latest".to_string(),
                    },
                    Tag {
                        name: "name".to_string(),
                        version: "foo".to_string(),
                    },
                ]
                .into_iter()
                .collect(),
            };
            assert!(matches!(result, Ok(actual) if actual == vec![expected]));
        }

        #[tokio::test]
        async fn should_find_multiple_matches() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(vec![json!(
                    [
                        {"Id": "alg:123", "RepoTags": ["name:latest", "name:foo"]},
                        {"Id": "alg:456", "RepoTags": ["name:foo"]},
                    ]
                )]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.where_named("foo").await;

            let expected = vec![
                Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "123".to_string(),
                    },
                    tags: vec![
                        Tag {
                            name: "name".to_string(),
                            version: "foo".to_string(),
                        },
                        Tag {
                            name: "name".to_string(),
                            version: "latest".to_string(),
                        },
                    ]
                    .into_iter()
                    .collect(),
                },
                Image {
                    id: Id {
                        algorithm: "alg".to_string(),
                        hex: "456".to_string(),
                    },
                    tags: vec![Tag {
                        name: "name".to_string(),
                        version: "foo".to_string(),
                    }]
                    .into_iter()
                    .collect(),
                },
            ];

            assert!(matches!(result, Ok(actual) if actual == expected));
        }
    }

    mod pull {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn shoud_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=latest&fromImage=foo",
                None,
                None,
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn shoud_error_if_received_got_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created_with_none(
                "/images/create?tag=latest&fromImage=foo",
                None,
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_got_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_post_and_return_no_content("/images/create?tag=latest&fromImage=foo", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_internal_server_error(
                "/images/create?tag=latest&fromImage=foo",
                None,
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_post_and_return_not_found("/images/create?tag=latest&fromImage=foo", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_error_if_docker_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=latest&fromImage=foo",
                None,
                Some(vec![json!({"error":"Oops all errors"})]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;

            assert!(matches!(result, Err(DockerError::ApiError(msg)) if msg == "Oops all errors"));
        }

        #[tokio::test]
        async fn should_pull_latest() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=latest&fromImage=foo",
                None,
                Some(vec![
                    json!({"status":"Pulling from library/foo","id":"latest"}),
                ]),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/images/foo:latest/json",
                Some(vec![json!({
                "Id": "alg:123456",
                  "RepoTags":["foo:latest"],
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.pull("foo", Version::Latest).await;
            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                },
                tags: vec![Tag {
                    name: "foo".to_string(),
                    version: "latest".to_string(),
                }]
                .into_iter()
                .collect(),
            };

            assert!(matches!(result, Ok(actual) if expected == actual));
        }

        #[tokio::test]
        async fn should_pull_specific_version() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=1.2.3&fromImage=foo",
                None,
                Some(vec![
                    json!({"status":"Pulling from library/foo","id":"latest"}),
                ]),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/images/foo:1.2.3/json",
                Some(vec![json!({
                "Id": "alg:123456",
                  "RepoTags":["foo:1.2.3"],
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .pull("foo", Version::Specific("1.2.3".to_string()))
                .await;

            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                },
                tags: vec![Tag {
                    name: "foo".to_string(),
                    version: "1.2.3".to_string(),
                }]
                .into_iter()
                .collect(),
            };
            assert!(matches!(result, Ok(actual) if expected == actual));
        }
    }

    mod inspect_by_name {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_error_if_body_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/foo:latest/json", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/images/foo:latest/json");
            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/images/foo:latest/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/images/foo:latest/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/images/foo:latest/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_error_if_too_little_json() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/foo:latest/json", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {status: 200, message, ..}) if message == "no results"
            ));
        }

        #[tokio::test]
        async fn should_error_if_too_much_json() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/foo:latest/json",
                Some(vec![
                    json!({
                    "Id": "alg:123456",
                      "RepoTags":["foo:latest"],
                    }),
                    json!({
                    "Id": "alg:987654",
                      "RepoTags":["foo:1.2.3"],
                    }),
                ]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {status: 200, message, ..}) if message == "too many results"
            ));
        }

        #[tokio::test]
        async fn should_inspect_latest() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/foo:latest/json",
                Some(vec![json!({
                "Id": "alg:123456",
                  "RepoTags":["foo:latest"],
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client.inspect_by_name("foo", Version::Latest).await;
            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                },
                tags: vec![Tag {
                    name: "foo".to_string(),
                    version: "latest".to_string(),
                }]
                .into_iter()
                .collect(),
            };

            assert!(matches!(result, Ok(actual) if expected == actual));
        }

        #[tokio::test]
        async fn should_inspect_specific() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/foo:1.2.3/json",
                Some(vec![json!({
                "Id": "alg:123456",
                  "RepoTags":["foo:1.2.3"],
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_name("foo", Version::Specific("1.2.3".to_string()))
                .await;

            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                },
                tags: vec![Tag {
                    name: "foo".to_string(),
                    version: "1.2.3".to_string(),
                }]
                .into_iter()
                .collect(),
            };

            assert!(matches!(result, Ok(actual) if expected == actual));
        }
    }

    mod inspect_by_id {
        use super::super::*;
        use serde_json::json;
        use simple_rest_client::MockRestClient;

        #[tokio::test]
        async fn should_err_if_multiple_chunks() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:123456/json",
                Some(vec![json!("too"), json!("many")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    message,
                    ..
                }) if message == "too many results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_missing_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/alg:123456/json", None);

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/alg:123456/json", Some(vec![]));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                    result,
                    Err(DockerError::UnexpectedResponseError {status: 200, message, ..}) if message == "no results"
            ));
        }

        #[tokio::test]
        async fn should_err_if_too_much_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:123456/json",
                Some(vec![json!("too"), json!("much")]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                    result,
                    Err(DockerError::UnexpectedResponseError {status: 200, message, ..}) if message == "too many results"
            ));
        }
        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/images/alg:123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/images/alg:123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_errors() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/images/alg:123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_err_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/images/alg:123456/json");

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_inspect_by_id() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:123456/json",
                Some(vec![json!({
                "Id": "alg:123456",
                  "RepoTags":["foo:1.2.3"],
                })]),
            );

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = client
                .inspect_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;
            let expected = Image {
                id: Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                },
                tags: vec![{
                    Tag {
                        name: "foo".to_string(),
                        version: "1.2.3".to_string(),
                    }
                }]
                .into_iter()
                .collect(),
            };

            assert!(matches!(result, Ok(actual) if expected == actual));
        }
    }
}
