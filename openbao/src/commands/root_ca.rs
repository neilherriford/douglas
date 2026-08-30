use crate::{
    Error,
    commands::{DataWrapper, open_bao_token_header},
};
use log::{Reporter, Span};
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize)]
struct Config {
    common_name: String,
    ttl: String,
}

#[derive(Debug, Deserialize)]
struct Response {
    certificate: String,
}

pub async fn generate<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    common_name: &str,
) -> Result<String, Error> {
    let guard = Span::new(reporter, "OpenBao generate root CA", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: "/v1/pki/root/generate/internal".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&Config {
            common_name: common_name.to_string(),
            ttl: "87600h".to_string(),
        })?),
        query: HashMap::new(),
    };

    let body = assert_okay_with_body(rest_client.execute(guard.span(), &req).await?)?;
    let parsed = serde_json::from_value::<DataWrapper<Response>>(parser.parse(body)?)?;

    guard.finish(Ok(parsed.data.certificate))
}

pub async fn is_configured<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
) -> Result<bool, Error> {
    let guard =
        Span::new(reporter, "OpenBao check PKI issuers", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: "/v1/pki/config/issuers".to_string(),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::new(),
    };

    let body = assert_okay_with_body(rest_client.execute(guard.span(), &req).await?)?;
    let parsed = serde_json::from_value::<DataWrapper<IssuersConfig>>(parser.parse(body)?)?;
    guard.finish(Ok(!parsed.data.default.is_empty()))
}

#[derive(Debug, Deserialize)]
struct IssuersConfig {
    default: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    #[tokio::test]
    async fn generate_should_post_a_long_lived_root_ca_and_return_the_certificate() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/pki/root/generate/internal"
                            && body.as_deref()
                                == Some(r#"{"common_name":"douglas","ttl":"87600h"}"#)
                )
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(
                        r#"{"data":{"certificate":"-----BEGIN CERTIFICATE-----"}}"#.to_string(),
                    ),
                })
            });

        let certificate = generate(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            "douglas",
        )
        .await
        .expect("should generate the root CA");

        assert_eq!(certificate, "-----BEGIN CERTIFICATE-----");
    }

    #[tokio::test]
    async fn is_configured_should_be_false_when_no_default_issuer_is_set() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Okay {
                headers: Vec::new(),
                body: Some(r#"{"data":{"default":""}}"#.to_string()),
            })
        });

        let configured = is_configured(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should check issuer config");

        assert!(!configured);
    }

    #[tokio::test]
    async fn is_configured_should_be_true_when_a_default_issuer_is_set() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Okay {
                headers: Vec::new(),
                body: Some(r#"{"data":{"default":"issuer-id"}}"#.to_string()),
            })
        });

        let configured = is_configured(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should check issuer config");

        assert!(configured);
    }
}
