use crate::Error;
use log::{Reporter, Span};
use openbao_types::Status;
use serde_json::from_value;
use simple_rest_client::{
    Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
) -> Result<Status, Error> {
    let guard = Span::new(reporter, "OpenBao status", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: "/v1/sys/health".to_string(),
        headers: vec![],
        query: HashMap::from([
            ("uninitcode".to_string(), "200".to_string()),
            ("sealedcode".to_string(), "200".to_string()),
            ("standbyok".to_string(), "true".to_string()),
        ]),
    };

    let body = assert_okay_with_body(rest_client.execute(guard.span(), &req).await?)?;

    guard.finish(match parser.parse(body) {
        Ok(json) => Ok(from_value(json)?),
        Err(err) => Err(Error::ParseError {
            line: 0,
            column: 0,
            message: format!("{err}"),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    fn healthy_status_body(sealed: bool) -> String {
        serde_json::json!({
            "initialized": true,
            "sealed": sealed,
            "standby": false,
            "performance_standby": false,
            "replication_performance_mode": "disabled",
            "replication_dr_mode": "disabled",
            "server_time_utc": 1,
            "version": "2.4.3",
        })
        .to_string()
    }

    #[tokio::test]
    async fn execute_should_ask_openbao_to_report_state_via_200_regardless_of_actual_state() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Get { path, headers, query }
                        if path == "/v1/sys/health"
                            && headers.is_empty()
                            && query
                                == &HashMap::from([
                                    ("uninitcode".to_string(), "200".to_string()),
                                    ("sealedcode".to_string(), "200".to_string()),
                                    ("standbyok".to_string(), "true".to_string()),
                                ])
                )
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(healthy_status_body(false)),
                })
            });

        let status = execute(Arc::new(NullReporter), &mut rest_client, &JsonParser::new())
            .await
            .expect("should parse status");

        assert!(status.initialized);
        assert!(!status.sealed);
    }

    #[tokio::test]
    async fn execute_should_parse_a_sealed_status_reported_as_200() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Okay {
                headers: Vec::new(),
                body: Some(healthy_status_body(true)),
            })
        });

        let status = execute(Arc::new(NullReporter), &mut rest_client, &JsonParser::new())
            .await
            .expect("should parse status");

        assert!(status.sealed);
    }

    #[tokio::test]
    async fn execute_should_fail_on_an_unexpected_error_response() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                headers: Vec::new(),
                status: 500,
                body: None,
            })
        });

        let result = execute(Arc::new(NullReporter), &mut rest_client, &JsonParser::new()).await;

        assert!(matches!(
            result,
            Err(Error::ClientResponseError(
                simple_rest_client::assertions::AssertionError::UnexpectedResponseError {
                    status: 500,
                    ..
                }
            ))
        ));
    }
}
