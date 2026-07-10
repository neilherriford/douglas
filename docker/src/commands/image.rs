use super::{assert_no_docker_errors, assert_non_empty_string_argument};
use crate::Image;
use crate::{DockerError, Id, VersionedImageName};
use log::{Reporter, Span};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::assert_okay_with_body;
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Request, RestClient};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ImageCommand {
    reporter: Arc<dyn Reporter>,
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
    single_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
}

impl ImageCommand {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        single_parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
        chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            single_parser,
            chunked_parser,
        }
    }

    pub async fn find_by_id(&mut self, id: &Id) -> Result<Image, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find image by name",
            log::ScopeKind::Task,
        )
        .start_guard();

        assert_non_empty_string_argument("id.algorithm", &id.algorithm)?;
        assert_non_empty_string_argument("id.hex", &id.hex)?;

        let request = Request::Get {
            path: format!("/images/{id}/json"),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.single_parser.parse(body)?;

        guard.finish(Ok(from_value(json)?))
    }

    pub async fn find_by_name(
        &mut self,
        image_name: &VersionedImageName,
    ) -> Result<Image, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find image by name",
            log::ScopeKind::Task,
        )
        .start_guard();
        assert_non_empty_string_argument("name", &image_name.name)?;

        let request = Request::Get {
            path: format!("/images/{image_name}/json"),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.single_parser.parse(body)?;

        guard.finish(Ok(from_value(json)?))
    }

    pub async fn list(&mut self) -> Result<Vec<Image>, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "List images",
            log::ScopeKind::Task,
        )
        .start_guard();

        let request = Request::Get {
            path: "/images/json".to_string(),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &request).await?;
        let body = assert_okay_with_body(response)?;
        let chunks = self.chunked_parser.parse(body)?;

        guard.finish(
            chunks
                .into_iter()
                .map(from_value::<Vec<Image>>)
                .collect::<Result<Vec<Vec<Image>>, _>>()
                .map(|vecs| vecs.into_iter().flatten().collect())
                .map_err(Into::into),
        )
    }

    pub async fn pull(&mut self, image_name: &VersionedImageName) -> Result<Image, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Pull image",
            log::ScopeKind::Task,
        )
        .start_guard();

        let request = Request::Post {
            path: simple_rest_client::create_path_and_query_string(
                "/images/create",
                HashMap::from([
                    ("fromImage", image_name.formatted_name().as_str()),
                    ("tag", &image_name.version.to_string()),
                ]),
            ),
            body: None,
            headers: vec![],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(guard.span(), &request).await?
        };

        let body = assert_okay_with_body(response)?;
        let chunks = self.chunked_parser.parse(body)?;
        assert_no_docker_errors(chunks)?;

        guard.finish(self.find_by_name(image_name).await)
    }
}
