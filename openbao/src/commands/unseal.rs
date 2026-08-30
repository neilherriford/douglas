use crate::Error;
use log::{Level, Reporter, Span};
use openbao_types::Secret;
use rand::prelude::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::from_value;
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Deserialize, PartialEq)]
struct Response {
    #[serde(rename = "t")]
    threshold: u8,
    #[serde(rename = "n")]
    share_count: u8,
    progress: u8,
    sealed: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ErrorResponse {
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UnsealRequest {
    key: String,
}

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    secrets: &[Secret],
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao unseal", log::ScopeKind::Task).start_guard();
    let mut secrets = Vec::from(secrets);
    secrets.shuffle(&mut rand::rng());
    let mut key_attempt = 1;

    loop {
        let secret = secrets.pop().ok_or(Error::InsufficentSecrets)?;

        guard
            .span()
            .message(Level::Info, &format!("Applying key #{key_attempt}…"));

        let response = unseal(guard.span(), rest_client, parser, &secret.key).await?;
        if !response.sealed {
            break;
        }
        key_attempt += 1
    }

    guard.span().message(Level::Info, "OpenBao is unsealed!");

    guard.finish(Ok(()))
}

async fn unseal<'a>(
    span: &Span,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    key: &'a str,
) -> Result<Response, Error> {
    let req = Request::Post {
        path: "/v1/sys/unseal".to_string(),
        body: Some(serde_json::to_string(&UnsealRequest {
            key: key.to_string(),
        })?),
        headers: vec![Header::content_type_json()],
        query: HashMap::new(),
    };

    let response = rest_client.execute(span, &req).await?;

    if let simple_rest_client::Response::Error { body, .. } = response {
        if let Some(body) = body {
            let parsed: ErrorResponse = parse(parser, body)?;
            return Err(Error::UnsealError(parsed.errors));
        } else {
            return Err(Error::UnsealError(vec!["No error context".to_string()]));
        }
    }

    let body = assert_okay_with_body(response)?;
    parse(parser, body)
}

fn parse<T>(parser: &JsonParser, body: String) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    match parser.parse(body) {
        Ok(json) => Ok(from_value(json)?),
        Err(err) => Err(Error::ParseError {
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
    use simple_rest_client::MockRestClient;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn unseal_status_body(sealed: bool) -> String {
        serde_json::json!({ "t": 3, "n": 5, "progress": 1, "sealed": sealed }).to_string()
    }

    #[tokio::test]
    async fn execute_should_fail_when_there_are_no_keys_to_try() {
        let mut rest_client = MockRestClient::new();

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &Vec::new(),
        )
        .await;

        assert!(matches!(result, Err(Error::InsufficentSecrets)));
    }

    #[tokio::test]
    async fn execute_should_stop_applying_keys_once_unsealed() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().times(1).returning(|_, _| {
            Ok(simple_rest_client::Response::Okay {
                headers: Vec::new(),
                body: Some(unseal_status_body(false)),
            })
        });

        let secrets = vec![Secret {
            key: "key-a".to_string(),
            base64: "a".to_string(),
        }];

        execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &secrets,
        )
        .await
        .expect("should unseal");
    }

    #[tokio::test]
    async fn execute_should_keep_applying_keys_while_still_sealed() {
        let mut rest_client = MockRestClient::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_seen_by_mock = Arc::clone(&attempts);

        rest_client
            .expect_execute()
            .times(2)
            .returning(move |_, _| {
                let attempt = attempts_seen_by_mock.fetch_add(1, Ordering::SeqCst);
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(unseal_status_body(attempt == 0)),
                })
            });

        let secrets = vec![
            Secret {
                key: "key-a".to_string(),
                base64: "a".to_string(),
            },
            Secret {
                key: "key-b".to_string(),
                base64: "b".to_string(),
            },
        ];

        execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &secrets,
        )
        .await
        .expect("should unseal after two attempts");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn execute_should_fail_when_the_server_rejects_a_key() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(simple_rest_client::Response::Error {
                headers: Vec::new(),
                status: 400,
                body: Some(r#"{"errors":["unseal key is not valid"]}"#.to_string()),
            })
        });

        let secrets = vec![Secret {
            key: "key-a".to_string(),
            base64: "a".to_string(),
        }];

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &secrets,
        )
        .await;

        assert!(matches!(
            result,
            Err(Error::UnsealError(errors)) if errors == vec!["unseal key is not valid".to_string()]
        ));
    }
}
