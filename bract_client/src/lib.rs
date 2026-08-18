use async_trait::async_trait;
use bract_types::{Request, SeedlingStatus};
use config::DouglasFolders;
use log::{Reporter, ScopeKind, Span};
use seedbank_types::{Name, SeedlingDefinition, SeedlingSpec, Version};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Missing socket. Has bract been started?")]
    MissingSocket,
    #[error("Connection refused. Is bract running?")]
    ConnectionRefused,
    #[error("Server closed the connection without responding")]
    NoResponse,
    #[error("Failed to serialize request: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    IoError(std::io::Error),
    #[error("Server error: {0}")]
    ServerError(String),
    #[error("Unexpected Response: {0}")]
    UnexpectedResponse(bract_types::Response),
}

#[async_trait]
pub trait Client: Send + Sync {
    async fn seedling_status(&self, name: &Name) -> Result<SeedlingStatus, Error>;
    async fn start_seedling(&self, name: &Name) -> Result<(), Error>;
    async fn stop_seedling(&self, name: &Name) -> Result<(), Error>;
    async fn drop_seedling(&self, name: &Name) -> Result<(), Error>;
    async fn reconcile_seedling(
        &self,
        name: &Name,
        version: &Version,
        seedling_definition: &SeedlingDefinition,
    ) -> Result<(), Error>;
    async fn new_seedling(
        &self,
        name: &Name,
        seedling_spec: &SeedlingSpec,
    ) -> Result<String, Error>;
}

pub struct UdsClient {
    reporter: Arc<dyn Reporter>,
    socket_path: PathBuf,
}

impl UdsClient {
    pub fn new(reporter: Arc<dyn Reporter>, douglas_folders: &DouglasFolders) -> Self {
        Self {
            reporter,
            socket_path: douglas_folders.socket_file("bract"),
        }
    }
    async fn request(
        &self,
        parent: &Span,
        request: Request,
    ) -> Result<bract_types::Response, Error> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(classify_connect_error)?;

        let mut serialized = serde_json::to_string(&request)?;
        serialized.push('\n');

        let (reader, mut writer) = stream.split();
        writer
            .write_all(serialized.as_bytes())
            .await
            .map_err(Error::IoError)?;

        let mut lines = BufReader::new(reader).lines();
        loop {
            let Some(line) = lines.next_line().await.map_err(Error::IoError)? else {
                return Err(Error::NoResponse);
            };

            match serde_json::from_str(&line)? {
                bract_types::ServerMessage::Event(mut event) => {
                    if event.parent_id.is_none() {
                        event.parent_id = Some(parent.id);
                    }
                    self.reporter.emit(event);
                }
                bract_types::ServerMessage::Response(response) => return Ok(response),
            }
        }
    }
}

#[async_trait]
impl Client for UdsClient {
    async fn seedling_status(&self, name: &Name) -> Result<SeedlingStatus, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Getting seedling status",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(guard.span(), Request::SeedlingStatus { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::SeedlingStatus(status) => Ok(status),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }

    async fn stop_seedling(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Stopping seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(guard.span(), Request::StopSeedling { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::Stopped => Ok(()),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }

    async fn drop_seedling(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Dropping seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(guard.span(), Request::DropSeedling { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::Dropped => Ok(()),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }

    async fn start_seedling(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(guard.span(), Request::StartSeedling { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::Started => Ok(()),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }

    async fn reconcile_seedling(
        &self,
        name: &Name,
        version: &Version,
        seedling_definition: &SeedlingDefinition,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Reconciling seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(
                guard.span(),
                Request::ReconcileSeedling {
                    name: name.clone(),
                    version: version.clone(),
                    seedling_definition: seedling_definition.clone(),
                },
            )
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::Started => Ok(()),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }

    async fn new_seedling(
        &self,
        name: &Name,
        seedling_spec: &SeedlingSpec,
    ) -> Result<String, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Creating seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(
                guard.span(),
                Request::NewSeedling {
                    name: name.clone(),
                    seedling_spec: seedling_spec.clone(),
                },
            )
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            bract_types::Response::Created { message } => Ok(message),
            bract_types::Response::Error { message } => Err(Error::ServerError(message)),
            unexpected => Err(Error::UnexpectedResponse(unexpected)),
        })
    }
}

fn classify_connect_error(err: std::io::Error) -> Error {
    match err.kind() {
        std::io::ErrorKind::NotFound => Error::MissingSocket,
        std::io::ErrorKind::ConnectionRefused => Error::ConnectionRefused,
        _ => Error::IoError(err),
    }
}
