use crate::{
    Error,
    commands::{DataWrapper, open_bao_token_header},
};
use log::{Reporter, Span};
use openbao_types::{KvConfig, Mounts};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

fn ensure_key_value(mount: &Mounts) -> Result<(), Error> {
    if mount.engine_type() == "kv" {
        return Ok(());
    }
    Err(Error::Error(format!("{mount} is not a key value mount")))
}

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    mount: &Mounts,
    config: &KvConfig,
) -> Result<(), Error> {
    let guard = Span::new(
        reporter,
        "OpenBao configure key value mount",
        log::ScopeKind::Task,
    )
    .start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Post {
        path: format!("/v1/{mount}/config"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(config)?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn read<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    mount: &Mounts,
) -> Result<KvConfig, Error> {
    let guard = Span::new(
        reporter,
        "OpenBao read key value mount configuration",
        log::ScopeKind::Task,
    )
    .start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Get {
        path: format!("/v1/{mount}/config"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;
    let parsed = serde_json::from_value::<DataWrapper<KvConfig>>(json)?;

    guard.finish(Ok(parsed.data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};

    #[tokio::test]
    async fn execute_should_post_the_config_to_the_mounts_config_path() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/douglas/config"
                            && body.as_deref()
                                == Some(r#"{"max_versions":20,"cas_required":true}"#)
                )
            })
            .returning(|_, _| {
                Ok(Response::NoContent {
                    headers: Vec::new(),
                })
            });

        execute(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &Mounts::ManagedSecrets,
            &KvConfig::managed_secrets(),
        )
        .await
        .expect("should configure the managed secrets mount");
    }

    #[tokio::test]
    async fn execute_should_send_the_token_header() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { headers, .. }
                        if headers.contains(&open_bao_token_header("root-token"))
                )
            })
            .returning(|_, _| {
                Ok(Response::NoContent {
                    headers: Vec::new(),
                })
            });

        execute(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &Mounts::ManagedSecrets,
            &KvConfig::managed_secrets(),
        )
        .await
        .expect("should send the token");
    }

    #[tokio::test]
    async fn execute_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &Mounts::PublicKeyInfrastructure,
            &KvConfig::managed_secrets(),
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }

    #[tokio::test]
    async fn execute_should_fail_when_openbao_rejects_the_config() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                status: 403,
                headers: Vec::new(),
                body: None,
            })
        });

        let result = execute(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &Mounts::ManagedSecrets,
            &KvConfig::managed_secrets(),
        )
        .await;

        assert!(result.is_err());
    }

    fn okay_with_body(body: &str) -> Response {
        Response::Okay {
            headers: Vec::new(),
            body: Some(body.to_string()),
        }
    }

    #[tokio::test]
    async fn read_should_get_the_mounts_config_and_parse_it() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, .. } if path == "/v1/douglas/config")
            })
            .returning(|_, _| {
                Ok(okay_with_body(
                    r#"{"data":{"max_versions":20,"cas_required":true,"delete_version_after":"0s"}}"#,
                ))
            });

        let config = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::ManagedSecrets,
        )
        .await
        .expect("should read the config");

        assert_eq!(config, KvConfig::managed_secrets());
    }

    #[tokio::test]
    async fn read_should_report_an_unconfigured_mount_as_its_defaults() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(okay_with_body(
                r#"{"data":{"max_versions":0,"cas_required":false,"delete_version_after":"0s"}}"#,
            ))
        });

        let config = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::ManagedSecrets,
        )
        .await
        .expect("should read the config");

        assert_ne!(config, KvConfig::managed_secrets());
    }

    #[tokio::test]
    async fn read_should_send_the_token_header() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Get { headers, .. }
                        if headers.contains(&open_bao_token_header("root-token"))
                )
            })
            .returning(|_, _| {
                Ok(okay_with_body(
                    r#"{"data":{"max_versions":20,"cas_required":true}}"#,
                ))
            });

        read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::ManagedSecrets,
        )
        .await
        .expect("should send the token");
    }

    #[tokio::test]
    async fn read_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::PublicKeyInfrastructure,
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }

    #[tokio::test]
    async fn read_should_fail_when_openbao_rejects_the_request() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                status: 403,
                headers: Vec::new(),
                body: None,
            })
        });

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::ManagedSecrets,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_should_fail_when_the_body_is_not_a_config() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(okay_with_body(r#"{"data":{"unexpected":true}}"#)));

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &Mounts::ManagedSecrets,
        )
        .await;

        assert!(result.is_err());
    }
}
