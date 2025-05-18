use crate::{DockerError, Label, SimpleDockerClient, deserialize_labels};
use serde::Deserialize;
use simple_rest_client::Request;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Network {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

#[async_trait::async_trait]
pub trait Repository {
    async fn inspect_by_id(&mut self, id: String) -> Result<Network, DockerError>;
}

#[async_trait::async_trait]
impl Repository for SimpleDockerClient {
    async fn inspect_by_id(&mut self, id: String) -> Result<Network, DockerError> {
        let request = Request::Get {
            path: format!("/networks/{}", id),
            headers: None,
        };

        Ok(self.expect_single_chunk::<Network>(request).await?)
    }
}

#[cfg(test)]
mod tests {
    mod inspect_by_id {
        use super::super::*;
        use crate::DockerError::ParseError;
        use crate::network::Repository;
        use serde_json::json;
        use serde_json::value::Value as Json;
        use simple_rest_client::{MockRestClient, Response};

        #[tokio::test]
        async fn should_err_if_missing_body() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
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

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;
            assert!(matches!(result, Err(DockerError::MissingBodyError)));
        }

        #[tokio::test]
        async fn should_err_if_empty_body() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
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

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;
            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    body: None,
                    message,
                }) => {
                    assert_eq!("no results".to_string(), message)
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_err_if_too_many_chunks() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!("too"), json!("many")]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 200,
                    body: _,
                    message,
                }) => {
                    assert_eq!("too many results".to_string(), message)
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_err_if_bad_json() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!("oops")]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            assert!(matches!(
                result,
                Err(ParseError {
                    line: 0,
                    column: 0,
                    message: _,
                })
            ));
        }

        #[tokio::test]
        async fn should_err_if_created() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Created {
                        headers: vec![],
                        body: Some(vec![json!("oops")]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 201,
                    body: _,
                    message,
                }) => {
                    assert_eq!("expected OK, but recieved CREATED".to_string(), message)
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_err_if_no_content() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| Ok(Response::<Vec<Json>>::NoContent { headers: vec![] }));

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            match result {
                Err(DockerError::UnexpectedResponseError {
                    status: 204,
                    body: None,
                    message,
                }) => {
                    assert_eq!("expected OK, but recieved NO CONTENT".to_string(), message)
                }
                _ => unreachable!("Expeceted images to match!"),
            }
        }

        #[tokio::test]
        async fn should_err_if_error() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Error {
                        headers: vec![],
                        status: 500,
                        body: Some(vec![json!("oops")]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            assert!(matches!(
                result,
                Err(DockerError::UnexpectedResponseError {
                    status: 500,
                    body: _,
                    message: _,
                })
            ));
        }

        #[tokio::test]
        async fn should_inspect_by_id() {
            let mut mock_rest_client = MockRestClient::new();

            mock_rest_client
                .expect_execute()
                .withf(|req| {
                    if let Request::Get { path, .. } = req {
                        path == "/networks/10111213"
                    } else {
                        false
                    }
                })
                .times(1)
                .return_once(|_req| {
                    Ok(Response::<Vec<Json>>::Okay {
                        headers: vec![],
                        body: Some(vec![json!({
                            "Id": "10111213",
                            "Name": "qux",
                            "Labels": {
                              "quux": "corge"
                            }
                        })]),
                    })
                });

            let mut client = SimpleDockerClient {
                rest_client: Box::new(mock_rest_client),
            };

            let result = Repository::inspect_by_id(&mut client, "10111213".to_string()).await;

            match result {
                Ok(actual) => {
                    let expected = Network {
                        id: "10111213".to_string(),
                        name: "qux".to_string(),
                        labels: vec![Label {
                            name: "quux".to_string(),
                            value: "corge".to_string(),
                        }],
                    };
                    assert_eq!(expected, actual);
                }
                _ => unreachable!("Unexpeted result"),
            }
        }
    }
}
