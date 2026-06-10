use crate::server::{Request as ServerRequest, Response as ServerResponse};
use crate::{Mount, Service};
use file_system::FileReader;
use futures::{SinkExt, StreamExt};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use serde::Serialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub enum IoOperation<'a> {
    WriteFile {
        relative_path: &'a Path,
        contents: &'a str,
    },
}

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Missing socket. Has the system been initialized?")]
    MissingSocket,
    #[error("Missing token. Has the system been initialized?")]
    MissingToken,
    #[error("Invalid token")]
    InvalidToken,
    #[error("Connection refused.  Is douglas running?")]
    ConnectionRefused,
    #[error("Server closed connection without responding")]
    NoResponse,
    #[error("Server returned an unexpected response, expected {expected} but received {received}")]
    UnexpectedResponse {
        expected: String,
        received: String,
        details: String,
    },
    #[error("Error: {0}")]
    Error(String),
}

#[derive(Debug)]
pub struct Credential {
    pub user: String,
    pub user_id: u32,
    pub group: String,
    pub group_id: u32,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Status {
    pub token_path: PathBuf,
    pub mount_root: PathBuf,
    pub services: Vec<Service>,
}

type Transport = SerdeFramed<
    Framed<UnixStream, LengthDelimitedCodec>,
    ServerResponse,
    ServerRequest,
    Json<ServerResponse, ServerRequest>,
>;

pub struct Client {
    reporter: Arc<dyn Reporter>,
    file_reader: Box<dyn FileReader>,
    socket_path: PathBuf,
    token_path: PathBuf,
}

impl Client {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        file_reader: Box<dyn FileReader>,
        socket_path: PathBuf,
        token_path: PathBuf,
    ) -> Self {
        Self {
            reporter,
            file_reader,
            token_path,
            socket_path,
        }
    }

    fn get_token(&self) -> Result<String, ClientError> {
        let token_path = self.token_path.as_path();

        if !self.file_reader.exists(token_path) {
            return Err(ClientError::MissingToken);
        }

        match self.file_reader.read_all(token_path) {
            Ok(token) => Ok(token),
            Err(err) => Err(ClientError::Error(format!("{:?}", err))),
        }
    }

    pub async fn status(&self) -> Result<Status, ClientError> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Getting status",
            ScopeKind::Task,
        );

        let response = self
            .request(
                span,
                ServerRequest::Status {
                    token: self.get_token()?,
                },
            )
            .await?;

        if let ServerResponse::Status {
            token_path,
            mount_root,
            services,
        } = response
        {
            Ok(Status {
                token_path,
                mount_root,
                services,
            })
        } else {
            Err(self.create_unexpected_response("Status", response))
        }
    }

    async fn request(
        &self,
        span: Span,
        request: ServerRequest,
    ) -> Result<ServerResponse, ClientError> {
        self.streaming_request(span, request, |_| {}).await
    }

    async fn streaming_request(
        &self,
        span: Span,
        request: ServerRequest,
        mut on_progress: impl FnMut(&ServerResponse),
    ) -> Result<ServerResponse, ClientError> {
        let reporter = span.create_scoped_reporter();

        match UnixStream::connect(self.socket_path.as_path()).await {
            Ok(stream) => {
                reporter.message(log::Level::Info, "Stream open");
                let length_delimited = Framed::new(stream, LengthDelimitedCodec::new());
                let mut transport: Transport = SerdeFramed::new(length_delimited, Json::default());

                if let Err(err) = transport.send(request).await {
                    let error_text = format!("{:?}", err);
                    reporter.message(log::Level::Warn, &error_text);
                    return Err(ClientError::Error(error_text));
                }

                loop {
                    match transport.next().await {
                        Some(Ok(response)) => {
                            if response.is_terminal() {
                                reporter.finish(Outcome::Ok);
                                return Ok(response);
                            }
                            on_progress(&response);
                        }
                        Some(Err(err)) => {
                            let error_text = format!("{:?}", err);
                            reporter.message(log::Level::Warn, &error_text);
                            return Err(ClientError::Error(error_text));
                        }
                        None => {
                            reporter.message(
                                Level::Warn,
                                "Server closed connection without responding",
                            );
                            return Err(ClientError::NoResponse);
                        }
                    }
                }
            }
            Err(err) => match err.kind() {
                ErrorKind::NotFound => {
                    reporter.message(Level::Warn, "Missing socket file");
                    Err(ClientError::MissingSocket)
                }
                ErrorKind::ConnectionRefused => {
                    reporter.message(Level::Warn, "Connection refused");
                    Err(ClientError::ConnectionRefused)
                }
                _ => {
                    let error_text = format!("{:?}", err);
                    reporter.message(Level::Warn, &error_text);
                    Err(ClientError::Error(error_text))
                }
            },
        }
    }

    pub(crate) fn create_unexpected_response(
        &self,
        expected: &str,
        response: ServerResponse,
    ) -> ClientError {
        todo!();
    }
}
