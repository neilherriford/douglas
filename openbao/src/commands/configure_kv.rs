use crate::{Error, commands::open_bao_token_header};
use log::{Reporter, Span};
use openbao_types::{KvConfig, Mounts};
use simple_rest_client::{Header, Request, RestClient, assertions::assert_okay_or_no_content};
use std::{collections::HashMap, sync::Arc};

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

    if mount.engine_type() != "kv" {
        return guard.finish(Err(Error::Error(format!(
            "{mount} is not a key value mount"
        ))));
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
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

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
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

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
}
