use crate::{Error, commands::open_bao_token_header};
use log::{Reporter, Span};
use serde::Serialize;
use simple_rest_client::{Header, Request, RestClient, assertions::assert_okay_or_no_content};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize)]
struct Config {
    path: String,
    aia_path: String,
}

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    base_url: &str,
) -> Result<(), Error> {
    let guard = Span::new(
        reporter,
        "OpenBao configure cluster path",
        log::ScopeKind::Task,
    )
    .start_guard();

    let req = Request::Post {
        path: "/v1/pki/config/cluster".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&Config {
            path: format!("{base_url}/v1/pki"),
            aia_path: format!("{base_url}/v1/pki"),
        })?),
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
    async fn execute_should_post_the_cluster_and_aia_paths_derived_from_the_base_url() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/pki/config/cluster"
                            && body.as_deref()
                                == Some(r#"{"path":"http://doug.openbao:8201/v1/pki","aia_path":"http://doug.openbao:8201/v1/pki"}"#)
                )
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        execute(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "http://doug.openbao:8201",
        )
        .await
        .expect("should configure the cluster path");
    }
}
