// openbao/src/commands/create_role.rs
use crate::{Error, commands::open_bao_token_header};
use log::{Reporter, Span};
use serde::Serialize;
use simple_rest_client::{
    Header, Request, Response, RestClient,
    assertions::{assert_okay_or_no_content, assert_okay_with_body},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize)]
struct Config {
    allowed_domains: String,
    allow_subdomains: bool,
    max_ttl: String,
}

pub async fn create<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    role_name: &str,
    allowed_domains: &str,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao create PKI role", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: format!("/v1/pki/roles/{role_name}"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&Config {
            allowed_domains: allowed_domains.to_string(),
            allow_subdomains: true,
            max_ttl: "8760h".to_string(),
        })?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn exists<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    role_name: &str,
) -> Result<bool, Error> {
    let guard = Span::new(reporter, "OpenBao check PKI role", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: format!("/v1/pki/roles/{role_name}"),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::new(),
    };

    match rest_client.execute(guard.span(), &req).await? {
        Response::Error { status: 404, .. } => guard.finish(Ok(false)),
        response => {
            assert_okay_with_body(response)?;
            guard.finish(Ok(true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::MockRestClient;
    use std::sync::Arc;

    #[tokio::test]
    async fn create_should_post_allowed_domains_with_subdomains_allowed() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/pki/roles/traefik"
                            && body.as_deref()
                                == Some(r#"{"allowed_domains":"localhost","allow_subdomains":true,"max_ttl":"8760h"}"#)
                )
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        create(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "traefik",
            "localhost",
        )
        .await
        .expect("should create the PKI role");
    }

    #[tokio::test]
    async fn exists_should_return_false_on_a_404() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                headers: Vec::new(),
                status: 404,
                body: None,
            })
        });

        let result = exists(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "traefik",
        )
        .await
        .expect("should check existence");

        assert!(!result);
    }

    #[tokio::test]
    async fn exists_should_return_true_when_found() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Okay {
                headers: Vec::new(),
                body: Some(r#"{"data":{}}"#.to_string()),
            })
        });

        let result = exists(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "traefik",
        )
        .await
        .expect("should check existence");

        assert!(result);
    }
}
