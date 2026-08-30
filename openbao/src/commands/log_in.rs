use crate::{Error, commands::AuthWrapper};
use log::{Reporter, Span};
use openbao_types::AuthType;
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    auth_type: &'a AuthType,
    name: &'a str,
    secret: &'a str,
) -> Result<String, Error> {
    let guard = Span::new(reporter, "OpenBao log in", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: format!("/v1/auth/{auth_type}/login"),
        headers: vec![Header::content_type_json()],
        body: Some(serde_json::to_string(&LoginRequest {
            role_id: name.into(),
            secret_id: secret.into(),
        })?),
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;
    let parsed = serde_json::from_value::<AuthWrapper<LoginData>>(json)?;

    guard.finish(Ok(parsed.auth.client_token))
}

#[derive(Debug, PartialEq, Serialize)]
struct LoginRequest {
    role_id: String,
    secret_id: String,
}

#[derive(Debug, PartialEq, Deserialize)]
struct LoginData {
    client_token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use openbao_types::AuthType;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    #[tokio::test]
    async fn execute_should_post_the_role_and_secret_and_return_the_client_token() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/auth/approle/login"
                            && body.as_deref() == Some(r#"{"role_id":"role-1","secret_id":"secret-1"}"#)
                )
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"auth":{"client_token":"the-token"}}"#.to_string()),
                })
            });

        let token = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &AuthType::AppRole,
            "role-1",
            "secret-1",
        )
        .await
        .expect("should log in");

        assert_eq!(token, "the-token");
    }

    #[tokio::test]
    async fn execute_should_fail_on_bad_credentials() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                headers: Vec::new(),
                status: 400,
                body: None,
            })
        });

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            &AuthType::AppRole,
            "role-1",
            "wrong-secret",
        )
        .await;

        assert!(result.is_err());
    }
}
