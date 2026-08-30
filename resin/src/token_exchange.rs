use async_trait::async_trait;
use docker_types::{ImagePathComponent, ImagePathComponentError};
use log::{Outcome, Reporter, ScopeKind, Span};
use resin_types::Name;
use serde::Deserialize;
use simple_rest_client::{
    Request, RestClient, RestClientError, ServerClosedConnections,
    assertions::{AssertionError, assert_okay_with_body},
    tls_socket,
};
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TokenExchangeError {
    #[error("Client build error {0}")]
    ClientBuildError(#[from] tls_socket::BuilderError),
    #[error("Rest client error {0}")]
    RestClientError(#[from] RestClientError),
    #[error("Response assertion failed {0}")]
    ResponseAssertionFailed(#[from] AssertionError),
    #[error("Unexpected response body {0}")]
    UnexpectedResponseBody(#[from] serde_json::Error),
    #[error("Invalid repository name {0}")]
    InvalidRepositoryName(#[from] ImagePathComponentError),
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait TokenExchange: Send + Sync {
    async fn fetch_token(&self, name: &Name) -> Result<String, TokenExchangeError>;
}

pub struct DockerHubTokenExchange {
    reporter: Arc<dyn Reporter>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
}

impl DockerHubTokenExchange {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self { reporter }
    }

    fn repository_name(name: &Name) -> Result<String, TokenExchangeError> {
        let namespace = match name.namespace() {
            Some(namespace) => namespace
                .to_string()
                .parse::<ImagePathComponent>()?
                .to_string(),
            None => "library".to_string(),
        };
        let component: ImagePathComponent = name.name().to_string().parse()?;

        Ok(format!("{namespace}/{component}"))
    }
}

#[async_trait]
impl TokenExchange for DockerHubTokenExchange {
    async fn fetch_token(&self, name: &Name) -> Result<String, TokenExchangeError> {
        let repository_name = Self::repository_name(name)?;

        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Fetching token",
            ScopeKind::Task,
        )
        .start_guard();
        let request = Request::Get {
            path: format!(
                "/token?service=registry.docker.io&scope=repository:{repository_name}:pull"
            ),
            headers: vec![],
            query: HashMap::new(),
        };

        let result: Result<String, TokenExchangeError> = async {
            let mut rest_client = Box::new(
                tls_socket::build_client("auth.docker.io", ServerClosedConnections::Ignore).await?,
            );

            let response = rest_client.execute(guard.span(), &request).await?;
            let body = assert_okay_with_body(response)?;
            let parsed: TokenResponse = serde_json::from_str(&body)?;

            Ok(parsed.token)
        }
        .await;

        guard.finish_with_outcome(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Failed
        });

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod repository_name {
        use super::*;

        #[test]
        fn test_repository_name_should_default_to_library_when_there_is_no_namespace() {
            let name: Name = "traefik".parse().unwrap();

            assert_eq!(
                DockerHubTokenExchange::repository_name(&name).unwrap(),
                "library/traefik"
            );
        }

        #[test]
        fn test_repository_name_should_use_the_namespace_as_is_when_present() {
            let name = Name::from_namespaced("someuser", "someimage").unwrap();

            assert_eq!(
                DockerHubTokenExchange::repository_name(&name).unwrap(),
                "someuser/someimage"
            );
        }
    }

    mod token_response {
        use super::*;

        #[test]
        fn test_token_response_should_deserialize_the_token_field() {
            let parsed: TokenResponse = serde_json::from_str(r#"{"token":"abc123"}"#).unwrap();

            assert_eq!(parsed.token, "abc123");
        }

        #[test]
        fn test_token_response_should_ignore_unknown_fields() {
            let parsed: TokenResponse = serde_json::from_str(
                r#"{"token":"abc123","access_token":"abc123","expires_in":300,"issued_at":"2024-01-01T00:00:00Z"}"#,
            )
            .unwrap();

            assert_eq!(parsed.token, "abc123");
        }

        #[test]
        fn test_token_response_should_fail_when_token_is_missing() {
            let result = serde_json::from_str::<TokenResponse>(r#"{"expires_in":300}"#);

            assert!(result.is_err());
        }
    }
}
