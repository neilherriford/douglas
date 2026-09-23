use async_trait::async_trait;
use log::{Outcome, Reporter, ScopeKind, Span};
use resin_types::{RepositoryPath, Upstream};
use serde::Deserialize;
use simple_rest_client::{
    Request, Response, RestClient, RestClientError, ServerClosedConnections,
    assertions::{AssertionError, assert_okay_with_body},
    create_path_and_query_string, header_predicates, tls_socket,
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
    #[error("Registry did not include a WWW-Authenticate challenge on its 401")]
    MissingChallenge,
    #[error("Could not parse WWW-Authenticate challenge: {0}")]
    UnparseableChallenge(String),
    #[error("Unexpected response discovering auth challenge: {0}")]
    UnexpectedChallengeResponse(String),
    #[error("Auth challenge realm is not a valid https URL: {0}")]
    InvalidRealm(String),
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait TokenExchange: Send + Sync {
    async fn fetch_token(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
    ) -> Result<Option<String>, TokenExchangeError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    realm: String,
    service: Option<String>,
}

impl BearerChallenge {
    pub fn parse(header_value: &str) -> Option<Self> {
        let parameters = header_value.strip_prefix("Bearer ")?;
        let mut realm = None;
        let mut service = None;
        for parameter in parameters.split(',') {
            let (key, value) = parameter.trim().split_once('=')?;
            match key {
                "realm" => realm = Some(value.trim_matches('"').to_string()),
                "service" => service = Some(value.trim_matches('"').to_string()),
                _ => {}
            }
        }
        Some(Self {
            realm: realm?,
            service,
        })
    }

    fn realm_host_and_path(&self) -> Option<(&str, &str)> {
        let without_scheme = self.realm.strip_prefix("https://")?;
        let boundary = without_scheme.find('/')?;
        Some((&without_scheme[..boundary], &without_scheme[boundary..]))
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
}

pub struct ChallengeTokenExchange {
    reporter: Arc<dyn Reporter>,
    challenges: tokio::sync::Mutex<HashMap<Upstream, Option<BearerChallenge>>>,
}

impl ChallengeTokenExchange {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self {
            reporter,
            challenges: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    async fn challenge_for(
        &self,
        upstream: &Upstream,
    ) -> Result<Option<BearerChallenge>, TokenExchangeError> {
        {
            let cache = self.challenges.lock().await;
            if let Some(cached) = cache.get(upstream) {
                return Ok(cached.clone());
            }
        }

        let challenge = self.discover_challenge(upstream).await?;

        let mut cache = self.challenges.lock().await;
        cache.insert(upstream.clone(), challenge.clone());
        Ok(challenge)
    }

    async fn discover_challenge(
        &self,
        upstream: &Upstream,
    ) -> Result<Option<BearerChallenge>, TokenExchangeError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Discovering auth challenge",
            ScopeKind::Task,
        )
        .start_guard();

        let request = Request::Get {
            path: "/v2/".to_string(),
            headers: vec![],
            query: HashMap::new(),
        };
        let api_host = upstream.api_host();

        let result: Result<Option<BearerChallenge>, TokenExchangeError> = async {
            let mut rest_client = Box::new(
                tls_socket::build_client(&api_host, ServerClosedConnections::Ignore).await?,
            );
            let response = rest_client.execute(guard.span(), &request).await?;

            match response {
                Response::Okay { .. } => Ok(None),
                Response::Error {
                    status: 401,
                    headers,
                    ..
                } => {
                    let value = headers
                        .iter()
                        .find(header_predicates::named("www-authenticate"))
                        .map(|header| header.value.clone())
                        .ok_or(TokenExchangeError::MissingChallenge)?;

                    BearerChallenge::parse(&value)
                        .ok_or(TokenExchangeError::UnparseableChallenge(value))
                        .map(Some)
                }
                other => Err(TokenExchangeError::UnexpectedChallengeResponse(
                    other.to_string(),
                )),
            }
        }
        .await;

        guard.finish_with_outcome(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Failed
        });

        result
    }

    async fn request_token(
        &self,
        challenge: &BearerChallenge,
        path: &RepositoryPath,
    ) -> Result<String, TokenExchangeError> {
        let (realm_host, realm_path) = challenge
            .realm_host_and_path()
            .ok_or_else(|| TokenExchangeError::InvalidRealm(challenge.realm.clone()))?;

        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Fetching token",
            ScopeKind::Task,
        )
        .start_guard();

        let scope = format!("repository:{path}:pull");
        let mut query = HashMap::new();
        if let Some(service) = &challenge.service {
            query.insert("service", service.as_str());
        }
        query.insert("scope", scope.as_str());

        let request = Request::Get {
            path: create_path_and_query_string(realm_path, query),
            headers: vec![],
            query: HashMap::new(),
        };

        let result: Result<String, TokenExchangeError> = async {
            let mut rest_client = Box::new(
                tls_socket::build_client(realm_host, ServerClosedConnections::Ignore).await?,
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

#[async_trait]
impl TokenExchange for ChallengeTokenExchange {
    async fn fetch_token(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
    ) -> Result<Option<String>, TokenExchangeError> {
        let Some(challenge) = self.challenge_for(upstream).await? else {
            return Ok(None);
        };
        self.request_token(&challenge, path).await.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod bearer_challenge {
        use super::*;

        #[test]
        fn test_parse_should_read_the_realm_and_service() {
            let challenge = BearerChallenge::parse(
                r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#,
            )
            .unwrap();

            assert_eq!(challenge.realm, "https://auth.docker.io/token");
            assert_eq!(challenge.service, Some("registry.docker.io".to_string()));
        }

        #[test]
        fn test_parse_should_accept_a_challenge_without_a_service() {
            let challenge =
                BearerChallenge::parse(r#"Bearer realm="https://ghcr.io/token""#).unwrap();

            assert_eq!(challenge.realm, "https://ghcr.io/token");
            assert_eq!(challenge.service, None);
        }

        #[test]
        fn test_parse_should_reject_a_non_bearer_scheme() {
            assert!(BearerChallenge::parse(r#"Basic realm="https://ghcr.io/token""#).is_none());
        }

        #[test]
        fn test_parse_should_reject_a_challenge_without_a_realm() {
            assert!(BearerChallenge::parse(r#"Bearer service="registry.docker.io""#).is_none());
        }

        #[test]
        fn test_realm_host_and_path_should_split_the_realm_url() {
            let challenge =
                BearerChallenge::parse(r#"Bearer realm="https://auth.docker.io/token""#).unwrap();

            assert_eq!(
                challenge.realm_host_and_path(),
                Some(("auth.docker.io", "/token"))
            );
        }

        #[test]
        fn test_realm_host_and_path_should_reject_a_non_https_realm() {
            let challenge = BearerChallenge {
                realm: "http://auth.docker.io/token".to_string(),
                service: None,
            };

            assert_eq!(challenge.realm_host_and_path(), None);
        }

        #[test]
        fn test_realm_host_and_path_should_reject_a_realm_without_a_path() {
            let challenge = BearerChallenge {
                realm: "https://auth.docker.io".to_string(),
                service: None,
            };

            assert_eq!(challenge.realm_host_and_path(), None);
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
