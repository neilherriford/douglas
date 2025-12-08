use super::{assert_no_docker_errors, assert_non_empty_string_argument, assert_okay_with_body};
use crate::Image;
use crate::{DockerError, Id, ImageName};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Request, RestClient};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ImageCommand {
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
    single_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
}

impl ImageCommand {
    pub fn new(
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        single_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
        chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            rest_client,
            single_parser,
            chunked_parser,
        }
    }

    pub async fn find_by_id(&mut self, id: &Id) -> Result<Image, DockerError> {
        assert_non_empty_string_argument("id.algorithm", &id.algorithm)?;
        assert_non_empty_string_argument("id.hex", &id.hex)?;

        let request = Request::Get {
            path: format!("/images/{id}/json"),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.single_parser.parse(body)?;

        Ok(from_value(json)?)
    }

    pub async fn find_by_name(&mut self, image_name: &ImageName) -> Result<Image, DockerError> {
        assert_non_empty_string_argument("name", &image_name.name)?;

        let request = Request::Get {
            path: format!(
                "/images/{}/{}:{}/json",
                image_name.namespace, image_name.name, image_name.version
            ),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.single_parser.parse(body)?;

        Ok(from_value(json)?)
    }

    pub async fn list(&mut self) -> Result<Vec<Image>, DockerError> {
        let request = Request::Get {
            path: "/images/json".to_string(),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(&request).await?;
        let body = assert_okay_with_body(response)?;
        let chunks = self.chunked_parser.parse(body)?;

        chunks
            .into_iter()
            .map(from_value::<Vec<Image>>)
            .collect::<Result<Vec<Vec<Image>>, _>>()
            .map(|vecs| vecs.into_iter().flatten().collect())
            .map_err(Into::into)
    }

    pub async fn pull(&mut self, image_name: &ImageName) -> Result<Image, DockerError> {
        let request = Request::Post {
            path: simple_rest_client::create_path_and_query_string(
                "/images/create",
                HashMap::from([
                    (
                        "fromImage",
                        format!("{}/{}", image_name.namespace, image_name.name).as_str(),
                    ),
                    ("tag", &image_name.version.to_string()),
                ]),
            ),
            body: None,
            headers: vec![],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(&request).await?
        };

        let body = assert_okay_with_body(response)?;
        let chunks = self.chunked_parser.parse(body)?;
        assert_no_docker_errors(chunks)?;

        self.find_by_name(image_name).await
    }
}

#[cfg(test)]
mod tests {
    mod tag_deserializer {
        use crate::{Tag, deserialize_tags};
        use serde::Deserialize;
        use std::collections::HashSet;

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
        use crate::{Tag, commands::json_parser::ChunkedJsonParser};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};

        #[tokio::test]
        async fn should_error_on_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/images/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_on_created_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/images/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_on_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_internal_server_error("/images/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.list().await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_return_error_if_the_json_is_unexpected() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay("/images/json", Some("Oops".to_string()));

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.list().await;
            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_list() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/json",
                Some(
                    r#"[{
                        "Containers": -1,
                        "Created": 1732220204,
                        "Id": "sha256:49891f502916212e83198a7e3425f99581a97e11762f462acd91c9a7b8d37f28",
                        "Labels": null,
                        "ParentId": "",
                        "RepoDigests":[
                            "example@sha256:888402a8cd6075c5dc83a31f58287f13306c318eaad016661ed12e076f3e6341"
                        ],
                        "RepoTags":["foo:bar"],
                        "SharedSize": -1,
                        "Size": 12345,
                        "VirtualSize": 67890
                    }]"#.to_string()
                )
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.list().await;
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

    mod pull {
        use crate::{
            DockerError, Id, Image, ImageName, Tag,
            commands::{ImageCommand, json_parser::ChunkedJsonParser},
        };
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn shoud_error_if_received_got_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_created(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
                None,
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_got_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_no_content(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_internal_server_error(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn shoud_error_if_received_missing() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_not_found(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_error_if_docker_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
                Some(r#"{"error":"Oops all errors"}"#.to_string()),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;

            assert!(matches!(result, Err(DockerError::ApiError(msg)) if msg == "Oops all errors"));
        }

        #[tokio::test]
        async fn should_pull_latest() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_post_and_return_okay(
                "/images/create?tag=latest&fromImage=namespace%2Ffoo",
                vec![],
                None,
                Some(r#"{"status":"Pulling from library/foo","id":"latest"}"#.to_string()),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/images/namespace/foo:latest/json",
                Some(
                    r#"{
                        "Id": "alg:123456",
                        "RepoTags":["foo:latest"]
                    }
                    "#
                    .to_string(),
                ),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command.pull(&ImageName::latest("namespace", "foo")).await;
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
                "/images/create?tag=1.2.3&fromImage=namespace%2Ffoo",
                vec![],
                None,
                Some(r#"{"status":"Pulling from library/foo","id":"latest"}"#.to_string()),
            );
            mock_rest_client.expect_get_and_return_okay(
                "/images/namespace/foo:1.2.3/json",
                Some(
                    r#"{
                        "Id": "alg:123456",
                        "RepoTags":["foo:1.2.3"]
                    }
                    "#
                    .to_string(),
                ),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .pull(&ImageName::specific("namespace", "foo", "1.2.3"))
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

    mod find_by_name {
        use super::super::*;
        use crate::{Tag, commands::json_parser::ChunkedJsonParser};
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};

        #[tokio::test]
        async fn should_err_if_name_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", ""))
                .await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "name" && given == String::new()
            ));
        }

        #[tokio::test]
        async fn should_error_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_created_with_none("/images/namespace/foo:latest/json");
            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", "foo"))
                .await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 201, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_no_content("/images/namespace/foo:latest/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", "foo"))
                .await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 204, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_error() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_internal_server_error("/images/namespace/foo:latest/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", "foo"))
                .await;
            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError { status: 500, .. })
            ));
        }

        #[tokio::test]
        async fn should_error_if_not_found() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_not_found("/images/namespace/foo:latest/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", "foo"))
                .await;
            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_inspect_latest() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/namespace/foo:latest/json",
                Some(r#"{"Id": "alg:123456", "RepoTags":["foo:latest"]}"#.to_string()),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::latest("namespace", "foo"))
                .await;
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
                "/images/namespace/foo:1.2.3/json",
                Some(r#"{"Id": "alg:123456", "RepoTags":["foo:1.2.3"]}"#.to_string()),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_name(&ImageName::specific("namespace", "foo", "1.2.3"))
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

    mod find_by_id {
        use crate::{
            DockerError, Id, Image, Tag,
            commands::{ImageCommand, json_parser::ChunkedJsonParser},
        };
        use simple_rest_client::{MockRestClient, parsers::json::JsonParser};
        use std::sync::Arc;

        #[tokio::test]
        async fn should_err_if_hex_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: String::new(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "id.hex" && given == String::new()
            ));
        }

        #[tokio::test]
        async fn should_err_if_alg_is_empty() {
            let mock_rest_client = MockRestClient::new();
            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
                    algorithm: String::new(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(
                result,
                Err(DockerError::InvalidArgumentError {
                    name,
                    given,
                    ..
                }) if name == "id.algorithm" && given == String::new()
            ));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client
                .expect_get_and_return_okay("/images/alg:123456/json", Some(String::new()));

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(result, Err(DockerError::ParseError { .. })));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_created_with_none("/images/alg:123456/json");

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
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

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
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

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
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

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string(),
                })
                .await;

            assert!(matches!(result, Err(DockerError::NotFoundError)));
        }

        #[tokio::test]
        async fn should_find_by_id() {
            let mut mock_rest_client = MockRestClient::new();
            mock_rest_client.expect_get_and_return_okay(
                "/images/alg:123456/json",
                Some(
                    r#"{
                        "Id": "alg:123456",
                        "RepoTags":["foo:1.2.3"]
                    }"#
                    .to_string(),
                ),
            );

            let mut command = ImageCommand::new(
                Arc::new(tokio::sync::Mutex::new(mock_rest_client)),
                Arc::new(JsonParser::new()),
                Arc::new(ChunkedJsonParser::new()),
            );

            let result = command
                .find_by_id(&Id {
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
