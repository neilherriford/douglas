use crate::{DockerError, client::ContainerRef, to_general_error};
use docker_types::{ExecId, ExecInspectionResult, ExecInstanceOptions, ExecStartOptions};
use log::{Reporter, Span};
use serde::Deserialize;
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{assert_created_with_body, assert_okay, assert_okay_with_body},
    parsers::{Parser, json::JsonParserError},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Deserialize, PartialEq)]
struct ExecResult {
    #[serde(rename = "Id")]
    id: String,
}

pub async fn create(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    container_ref: &ContainerRef,
    options: &ExecInstanceOptions,
) -> Result<ExecId, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Create exec instance",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request_body = serde_json::to_string(options).map_err(to_general_error)?;

    let request = Request::Post {
        path: format!("/containers/{container_ref}/exec"),
        headers: vec![Header::content_type_json()],
        body: Some(request_body),
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_created_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let result: ExecResult = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(result.id.into()))
}

pub async fn start(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    id: &ExecId,
    options: &ExecStartOptions,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Start an exec instance",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request_body = serde_json::to_string(options).map_err(to_general_error)?;

    let request = Request::Post {
        path: format!("/exec/{id}/start"),
        headers: vec![Header::content_type_json()],
        body: Some(request_body),
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    assert_okay(response)?;

    guard.finish(Ok(()))
}

pub async fn inspect(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    id: &ExecId,
) -> Result<ExecInspectionResult, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Inspect an exec instance",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Get {
        path: format!("/exec/{id}/json"),
        headers: vec![],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let result: ExecInspectionResult = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use docker_types::ContainerName;
    use log::Event;
    use simple_rest_client::{MockRestClient, Response};

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn reporter() -> Arc<dyn Reporter> {
        Arc::new(NullReporter)
    }

    fn container_ref() -> ContainerRef {
        ContainerRef::FullName("my-container".parse::<ContainerName>().unwrap())
    }

    #[tokio::test]
    async fn create_should_set_the_json_content_type_header() {
        let mut mock = MockRestClient::new();
        mock.expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { headers, .. } if headers.contains(&Header::content_type_json())
                )
            })
            .returning(|_, _| {
                Ok(Response::Created {
                    headers: Vec::new(),
                    body: Some(r#"{"Id":"abc123"}"#.to_string()),
                })
            });
        let rest_client = tokio::sync::Mutex::new(mock);

        create(
            reporter(),
            &rest_client,
            Arc::new(simple_rest_client::parsers::json::JsonParser::new()),
            &container_ref(),
            &ExecInstanceOptions {
                attach_stdin: false,
                attach_stdout: true,
                attach_stderr: true,
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
            },
        )
        .await
        .expect("should create the exec instance");
    }

    #[tokio::test]
    async fn start_should_set_the_json_content_type_header() {
        let mut mock = MockRestClient::new();
        mock.expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { headers, .. } if headers.contains(&Header::content_type_json())
                )
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: None,
                })
            });
        let rest_client = tokio::sync::Mutex::new(mock);

        start(
            reporter(),
            &rest_client,
            &ExecId::from("abc123".to_string()),
            &ExecStartOptions::default(),
        )
        .await
        .expect("should start the exec instance");
    }
}
