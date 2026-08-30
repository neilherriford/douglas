use crate::{Error, commands::open_bao_token_header};
use log::{Reporter, Span};
use serde::Serialize;
use simple_rest_client::{Header, Request, RestClient, assertions::assert_okay_or_no_content};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize)]
struct Config {
    issuing_certificates: Vec<String>,
    crl_distribution_points: Vec<String>,
}

pub async fn execute<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    base_url: &str,
) -> Result<(), Error> {
    let guard = Span::new(
        reporter,
        "OpenBao set issuing/CRL urls",
        log::ScopeKind::Task,
    )
    .start_guard();

    let req = Request::Post {
        path: "/v1/pki/config/urls".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&Config {
            issuing_certificates: vec![format!("{base_url}/v1/pki/ca")],
            crl_distribution_points: vec![format!("{base_url}/v1/pki/crl")],
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
    async fn execute_should_post_the_issuing_and_crl_urls_derived_from_the_base_url() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/pki/config/urls"
                            && body.as_deref()
                                == Some(r#"{"issuing_certificates":["http://doug.openbao:8201/v1/pki/ca"],"crl_distribution_points":["http://doug.openbao:8201/v1/pki/crl"]}"#)
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
        .expect("should configure the issuing/CRL urls");
    }
}
