use std::sync::Arc;

use async_trait::async_trait;
use docker_types::VersionedImageName;
use log::{Reporter, Span};
use simple_rest_client::{Request, Response, RestClient, RestClientError, tcp_socket};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Build error {0}")]
    BuildError(#[from] tcp_socket::BuilderError),
    #[error("REST error {0}")]
    RestError(#[from] RestClientError),
    #[error("Unexpected response {0}")]
    UnexpectedResponse(Response),
}

#[async_trait]
pub trait Client: Send + Sync {
    async fn image_exists(&mut self, image: VersionedImageName) -> Result<bool, Error>;
}

pub struct HttpClient {
    reporter: Arc<dyn Reporter>,
    client: Box<dyn RestClient>,
}

impl HttpClient {
    pub async fn build_for_localhost(reporter: Arc<dyn Reporter>) -> Result<Self, Error> {
        let client = tcp_socket::build_client(
            "localhost",
            resin_types::DEFAULT_PORT,
            simple_rest_client::ServerClosedConnections::Ignore,
        )
        .await?;

        Ok(Self {
            reporter,
            client: Box::new(client),
        })
    }

    fn manifest_path(image: VersionedImageName) -> String {
        match image.namespace {
            Some(namespace) => {
                format!("/v2/{namespace}/{}/manifests/{}", image.name, image.version)
            }
            None => format!("/v2/{}/manifests/{}", image.name, image.version),
        }
    }
}

#[async_trait]
impl Client for HttpClient {
    async fn image_exists(&mut self, image: VersionedImageName) -> Result<bool, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Checking image existence for {image}"),
            log::ScopeKind::Task,
        )
        .start_guard();

        let request = Request::Head {
            path: Self::manifest_path(image),
            headers: Vec::new(),
        };

        match self.client.execute(guard.span(), &request).await? {
            Response::Okay { .. } => Ok(true),
            Response::Error { status: 404, .. } => Ok(false),
            response => Err(Error::UnexpectedResponse(response)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Event;
    use simple_rest_client::MockRestClient;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn client(rest_client: MockRestClient) -> HttpClient {
        HttpClient {
            reporter: Arc::new(NullReporter),
            client: Box::new(rest_client),
        }
    }

    mod manifest_path {
        use super::*;

        #[test]
        fn test_should_omit_namespace_segment_when_absent() {
            let image = VersionedImageName::specific("nginx", "1.27");

            assert_eq!(HttpClient::manifest_path(image), "/v2/nginx/manifests/1.27");
        }

        #[test]
        fn test_should_include_namespace_segment_when_present() {
            let image = VersionedImageName::namespaced_specific("openbao", "openbao", "2.4.3");

            assert_eq!(
                HttpClient::manifest_path(image),
                "/v2/openbao/openbao/manifests/2.4.3"
            );
        }

        #[test]
        fn test_should_use_latest_as_the_reference_when_unversioned() {
            let image = VersionedImageName::latest("nginx");

            assert_eq!(
                HttpClient::manifest_path(image),
                "/v2/nginx/manifests/latest"
            );
        }
    }

    mod image_exists {
        use super::*;

        #[tokio::test]
        async fn test_should_return_true_when_manifest_is_found() {
            let mut rest_client = MockRestClient::new();
            rest_client
                .expect_execute()
                .withf(|_, request| {
                    matches!(request, Request::Head { path, .. } if path == "/v2/nginx/manifests/1.27")
                })
                .returning(|_, _| {
                    Ok(Response::Okay {
                        headers: Vec::new(),
                        body: None,
                    })
                });

            let mut client = client(rest_client);
            let image = VersionedImageName::specific("nginx", "1.27");

            assert!(
                client
                    .image_exists(image)
                    .await
                    .expect("should check existence")
            );
        }

        #[tokio::test]
        async fn test_should_return_false_when_manifest_is_missing() {
            let mut rest_client = MockRestClient::new();
            rest_client.expect_execute().returning(|_, _| {
                Ok(Response::Error {
                    headers: Vec::new(),
                    status: 404,
                    body: None,
                })
            });

            let mut client = client(rest_client);
            let image = VersionedImageName::specific("nginx", "1.27");

            assert!(
                !client
                    .image_exists(image)
                    .await
                    .expect("should check existence")
            );
        }

        #[tokio::test]
        async fn test_should_error_on_unexpected_response() {
            let mut rest_client = MockRestClient::new();
            rest_client.expect_execute().returning(|_, _| {
                Ok(Response::Error {
                    headers: Vec::new(),
                    status: 500,
                    body: None,
                })
            });

            let mut client = client(rest_client);
            let image = VersionedImageName::specific("nginx", "1.27");

            let result = client.image_exists(image).await;

            assert!(matches!(result, Err(Error::UnexpectedResponse(_))));
        }

        #[tokio::test]
        async fn test_should_propagate_rest_client_errors() {
            let mut rest_client = MockRestClient::new();
            rest_client
                .expect_execute()
                .returning(|_, _| Err(RestClientError::General("boom".to_string())));

            let mut client = client(rest_client);
            let image = VersionedImageName::specific("nginx", "1.27");

            let result = client.image_exists(image).await;

            assert!(matches!(result, Err(Error::RestError(_))));
        }
    }
}
