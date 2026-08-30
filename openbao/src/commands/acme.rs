use crate::{
    Error,
    commands::{DataWrapper, open_bao_token_header},
};
use log::{Reporter, Span};
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    enabled: bool,
}

pub async fn enable<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    enabled: bool,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao enable ACME", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: "/v1/pki/config/acme".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&Config { enabled })?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn is_enabled<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
) -> Result<bool, Error> {
    let guard =
        Span::new(reporter, "OpenBao check ACME config", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: "/v1/pki/config/acme".to_string(),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::new(),
    };

    let body = assert_okay_with_body(rest_client.execute(guard.span(), &req).await?)?;
    let parsed = serde_json::from_value::<DataWrapper<Config>>(parser.parse(body)?)?;
    guard.finish(Ok(parsed.data.enabled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    #[tokio::test]
    async fn enable_should_post_the_desired_enabled_flag() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/pki/config/acme" && body.as_deref() == Some(r#"{"enabled":true}"#)
                )
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        enable(Arc::new(NullReporter), &mut rest_client, "root-token", true)
            .await
            .expect("should enable ACME");
    }

    #[tokio::test]
    async fn is_enabled_should_parse_the_current_config() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| matches!(request, Request::Get { path, .. } if path == "/v1/pki/config/acme"))
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"enabled":true}}"#.to_string()),
                })
            });

        let enabled = is_enabled(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should parse ACME config");

        assert!(enabled);
    }
}
