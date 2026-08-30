use crate::Error;
use log::{Reporter, Span};
use openbao_types::{Secret, Secrets};
use serde::{Deserialize, Serialize};
use serde_json::from_value;
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, PartialEq, Serialize)]
struct Config {
    pub secret_shares: u8,
    pub secret_threshold: u8,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Status {
    initialized: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Response {
    keys: Vec<String>,
    keys_base64: Vec<String>,
    root_token: String,
}

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    secret_shares: u8,
    secret_threshold: u8,
) -> Result<Secrets, crate::Error> {
    let guard = Span::new(reporter, "OpenBao initialize", log::ScopeKind::Task).start_guard();

    if is_initialized(rest_client, parser, guard.span()).await? {
        return Err(crate::Error::AlreadyInitialized);
    }
    guard.finish(
        initialize(
            rest_client,
            parser,
            guard.span(),
            secret_shares,
            secret_threshold,
        )
        .await,
    )
}

async fn is_initialized<'a>(
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    span: &Span,
) -> Result<bool, crate::Error> {
    let req = Request::Get {
        path: "/v1/sys/init".to_string(),
        headers: vec![],
        query: HashMap::new(),
    };

    let body = assert_okay_with_body(rest_client.execute(span, &req).await?)?;

    match parser.parse(body) {
        Ok(json) => {
            let status: Status = from_value(json)?;
            Ok(status.initialized)
        }
        Err(err) => Err(crate::Error::ParseError {
            line: 0,
            column: 0,
            message: format!("{err}"),
        }),
    }
}

async fn initialize<'a>(
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    span: &Span,
    secret_shares: u8,
    secret_threshold: u8,
) -> Result<Secrets, Error> {
    if secret_shares == 0 {
        return Err(Error::SharesTooSmall);
    } else if secret_threshold == 0 {
        return Err(Error::ThresholdTooSmall);
    } else if secret_threshold > secret_shares {
        return Err(Error::InvalidThreshold);
    }

    let config = Config {
        secret_shares,
        secret_threshold,
    };
    let req = Request::Post {
        path: "/v1/sys/init".to_string(),
        headers: vec![Header::content_type_json()],
        body: Some(serde_json::to_string(&config)?),
        query: HashMap::new(),
    };

    let body = assert_okay_with_body(rest_client.execute(span, &req).await?)?;

    match parser.parse(body) {
        Ok(json) => {
            let response: Response = from_value(json)?;
            let secrets = (0..response.keys.len())
                .map(|index| Secret {
                    key: response.keys[index].clone(),
                    base64: response.keys_base64[index].clone(),
                })
                .collect();

            Ok(Secrets {
                secrets,
                root_token: response.root_token,
            })
        }
        Err(err) => Err(crate::Error::ParseError {
            line: 0,
            column: 0,
            message: format!("{err}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    fn not_yet_initialized(rest_client: &mut MockRestClient) {
        rest_client
            .expect_execute()
            .withf(
                |_, request| matches!(request, Request::Get { path, .. } if path == "/v1/sys/init"),
            )
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"initialized":false}"#.to_string()),
                })
            });
    }

    #[tokio::test]
    async fn execute_should_fail_when_already_initialized() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(
                |_, request| matches!(request, Request::Get { path, .. } if path == "/v1/sys/init"),
            )
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"initialized":true}"#.to_string()),
                })
            });

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            5,
            3,
        )
        .await;

        assert!(matches!(result, Err(crate::Error::AlreadyInitialized)));
    }

    #[tokio::test]
    async fn execute_should_reject_zero_shares() {
        let mut rest_client = MockRestClient::new();
        not_yet_initialized(&mut rest_client);

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            0,
            0,
        )
        .await;

        assert!(matches!(result, Err(crate::Error::SharesTooSmall)));
    }

    #[tokio::test]
    async fn execute_should_reject_a_zero_threshold() {
        let mut rest_client = MockRestClient::new();
        not_yet_initialized(&mut rest_client);

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            5,
            0,
        )
        .await;

        assert!(matches!(result, Err(crate::Error::ThresholdTooSmall)));
    }

    #[tokio::test]
    async fn execute_should_reject_a_threshold_greater_than_the_share_count() {
        let mut rest_client = MockRestClient::new();
        not_yet_initialized(&mut rest_client);

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            3,
            5,
        )
        .await;

        assert!(matches!(result, Err(crate::Error::InvalidThreshold)));
    }

    #[tokio::test]
    async fn execute_should_pair_up_keys_and_base64_keys_by_index() {
        let mut rest_client = MockRestClient::new();
        not_yet_initialized(&mut rest_client);
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/sys/init"
                            && body.as_deref() == Some(r#"{"secret_shares":5,"secret_threshold":3}"#)
                )
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(
                        r#"{"keys":["aa","bb"],"keys_base64":["AA==","BB=="],"root_token":"root"}"#
                            .to_string(),
                    ),
                })
            });

        let secrets = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            5,
            3,
        )
        .await
        .expect("should initialize");

        assert_eq!(secrets.root_token, "root");
        assert_eq!(
            secrets.secrets,
            vec![
                Secret {
                    key: "aa".to_string(),
                    base64: "AA==".to_string()
                },
                Secret {
                    key: "bb".to_string(),
                    base64: "BB==".to_string()
                },
            ]
        );
    }
}
