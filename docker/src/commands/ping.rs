use crate::{DockerError, PingResult};
use log::{Reporter, Span};
use simple_rest_client::{Request, RestClient, assertions::assert_okay_with_body, parsers::Parser};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PingParserError {}

#[derive(Debug, Default)]
pub struct PingParser {}

impl PingParser {
    pub fn new() -> Self {
        Self {}
    }
}

impl Parser<PingResult> for PingParser {
    type ParseError = PingParserError;

    fn parse(&self, input: String) -> Result<PingResult, Self::ParseError> {
        if input == "OK" {
            Ok(PingResult::Ok)
        } else {
            Ok(PingResult::Error(format!(
                "Unexpected ping value: '{input}'"
            )))
        }
    }
}

pub struct PingCommand {
    reporter: Arc<dyn Reporter>,
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + 'static>>,
    parser: Box<dyn Parser<PingResult, ParseError = PingParserError> + Send>,
}

impl PingCommand {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        parser: Box<dyn Parser<PingResult, ParseError = PingParserError> + Send>,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
        }
    }
}

impl PingCommand {
    pub async fn ping(&mut self) -> Result<PingResult, DockerError> {
        let guard =
            Span::new(Arc::clone(&self.reporter), "Ping", log::ScopeKind::Task).start_guard();
        let req = Request::Get {
            path: "/_ping".to_string(),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &req).await?;

        let body = assert_okay_with_body(response)?;

        guard.finish(match self.parser.parse(body) {
            Ok(result) => Ok(result),
            Err(err) => Err(DockerError::ParseError {
                line: 0,
                column: 0,
                message: format!("{err}"),
            }),
        })
    }
}
