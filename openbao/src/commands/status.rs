use std::sync::Arc;

use crate::{OpenBaoError, Status};
use log::{Reporter, Span};
use serde_json::from_value;
use simple_rest_client::{
    Request, RestClient,
    parsers::{Parser, json::JsonParser},
};

pub struct StatusCommand<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
}

impl<'a> StatusCommand<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
        }
    }

    pub async fn perform(&mut self) -> Result<Status, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao status",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Get {
            path: "/v1/sys/health".to_string(),
            headers: vec![],
        };

        let body = self.get_body(guard.span(), req).await?;

        guard.finish(match self.parser.parse(body) {
            Ok(json) => Ok(from_value(json)?),
            Err(err) => Err(OpenBaoError::ParseError {
                line: 0,
                column: 0,
                message: format!("{err}"),
            }),
        })
    }

    async fn get_body(&mut self, span: &Span, req: Request) -> Result<String, OpenBaoError> {
        let body = match self.rest_client.execute(span, &req).await? {
            simple_rest_client::Response::Okay { body, .. } => {
                if let Some(body) = body {
                    body
                } else {
                    return Err(OpenBaoError::UnexpectedResponse {
                        status: 200,
                        body: None,
                        message: "Expected a response body".to_string(),
                    });
                }
            }
            simple_rest_client::Response::Created { body, .. } => {
                if let Some(body) = body {
                    body
                } else {
                    return Err(OpenBaoError::UnexpectedResponse {
                        status: 201,
                        body: None,
                        message: "Expected a response body".to_string(),
                    });
                }
            }
            simple_rest_client::Response::NoContent { .. } => {
                return Err(OpenBaoError::UnexpectedResponse {
                    status: 204,
                    body: None,
                    message: "Expected a response body".to_string(),
                });
            }
            simple_rest_client::Response::Error { status, body, .. }
                if status == 501 || status == 503 =>
            {
                if let Some(body) = body {
                    body
                } else {
                    return Err(OpenBaoError::UnexpectedResponse {
                        status,
                        body: None,
                        message: "Expected a response body".to_string(),
                    });
                }
            }

            simple_rest_client::Response::Error { status, body, .. } => {
                return Err(OpenBaoError::UnexpectedResponse {
                    status,
                    body,
                    message: "Unexpected reponse".to_string(),
                });
            }
        };
        Ok(body)
    }
}
