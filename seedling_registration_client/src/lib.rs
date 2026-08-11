use async_trait::async_trait;
use config::DouglasFolders;
use log::{Reporter, ScopeKind, Span};
use seedling_registration_types::{Request, Response};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[cfg(feature = "mock")]
use mockall::automock;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Missing socket. Has seedbank been started?")]
    MissingSocket,
    #[error("Connection refused. Is seedbank running?")]
    ConnectionRefused,
    #[error("Server closed the connection without responding")]
    NoResponse,
    #[error("Failed to serialize request: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    IoError(std::io::Error),
}

#[cfg_attr(feature = "mock", automock)]
#[async_trait]
pub trait Client: Send + Sync {
    async fn seedling_registered(&self, name: &str) -> Result<Response, Error>;
}

pub struct UdsClient {
    reporter: Arc<dyn Reporter>,
    socket_path: PathBuf,
}

impl UdsClient {
    pub fn new(reporter: Arc<dyn Reporter>, douglas_folders: &DouglasFolders) -> Self {
        Self {
            reporter,
            socket_path: douglas_folders.socket_file(seedling_registration_types::SOCKET_NAME),
        }
    }

    async fn request(&self, request: Request) -> Result<Response, Error> {
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
        match lines.next_line().await.map_err(Error::IoError)? {
            Some(line) => Ok(serde_json::from_str(&line)?),
            None => Err(Error::NoResponse),
        }
    }
}

#[async_trait]
impl Client for UdsClient {
    async fn seedling_registered(&self, name: &str) -> Result<Response, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Checking seedling registration for '{name}'"),
            ScopeKind::Task,
        )
        .start_guard();

        let request = Request {
            name: name.to_string(),
        };

        guard.finish(self.request(request).await)
    }
}

fn classify_connect_error(err: std::io::Error) -> Error {
    match err.kind() {
        std::io::ErrorKind::NotFound => Error::MissingSocket,
        std::io::ErrorKind::ConnectionRefused => Error::ConnectionRefused,
        _ => Error::IoError(err),
    }
}
