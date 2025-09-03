use crate::Request as ServerRequest;
use crate::Response;
use crate::Version;
use file_system::{FileReader, FileSystemError};
use futures::{SinkExt, StreamExt};
use log::Logger;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Debug)]
pub enum Request {
    ActiveMountVersion {
        service_name: String,
        mount_name: String,
    },
    CreateCredentials {
        service_name: String,
    },
    CreateMount {
        service_name: String,
        mount_name: String,
    },
    CreateNewMountVersion {
        service_name: String,
        mount_name: String,
    },
    ListMountVersions {
        service_name: String,
        mount_name: String,
    },
    SetMountVersion {
        service_name: String,
        mount_name: String,
        version: Version,
    },
    Status,
    Shutdown,
}

struct ServerRequestFactory {
    file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
    token_path: PathBuf,
}

impl ServerRequestFactory {
    pub fn new(
        file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
        token_path: &Path,
    ) -> Self {
        Self {
            file_reader: Arc::clone(&file_reader),
            token_path: token_path.to_path_buf(),
        }
    }

    pub fn create(&self, request: Request) -> Result<ServerRequest, FileSystemError> {
        let token = self.file_reader.read_all(self.token_path.as_path())?;

        match request {
            Request::ActiveMountVersion {
                service_name,
                mount_name,
            } => Ok(ServerRequest::ActiveMountVersion {
                token,
                service_name,
                mount_name,
            }),
            Request::CreateCredentials { service_name } => Ok(ServerRequest::CreateCredentials {
                token,
                service_name,
            }),
            Request::CreateMount {
                service_name,
                mount_name,
            } => Ok(ServerRequest::CreateMount {
                token,
                service_name,
                mount_name,
            }),
            Request::CreateNewMountVersion {
                service_name,
                mount_name,
            } => Ok(ServerRequest::CreateNewMountVersion {
                token,
                service_name,
                mount_name,
            }),
            Request::ListMountVersions {
                service_name,
                mount_name,
            } => Ok(ServerRequest::ListMountVersions {
                token,
                service_name,
                mount_name,
            }),
            Request::SetMountVersion {
                service_name,
                mount_name,
                version,
            } => Ok(ServerRequest::SetMountVersion {
                token,
                service_name,
                mount_name,
                version,
            }),
            Request::Status => Ok(ServerRequest::Status { token }),
            Request::Shutdown => Ok(ServerRequest::Shutdown { token }),
        }
    }
}

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Server closed connection without responding")]
    NoResponse,
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub struct Client {
    server_request_factory: Arc<ServerRequestFactory>,
    log: Arc<dyn Logger + Sync + Send + 'static>,
    socket_path: PathBuf,
}

type Transport = SerdeFramed<
    Framed<UnixStream, LengthDelimitedCodec>,
    Response,
    ServerRequest,
    Json<Response, ServerRequest>,
>;

impl Client {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
        socket_path: &Path,
        token_path: &Path,
    ) -> Self {
        Self {
            log: Arc::clone(&log),
            server_request_factory: Arc::new(ServerRequestFactory::new(file_reader, token_path)),
            socket_path: socket_path.to_path_buf(),
        }
    }

    pub async fn request(&self, request: Request) -> Result<Response, ClientError> {
        let request = self.server_request_factory.create(request)?;
        let stream = UnixStream::connect(self.socket_path.as_path()).await?;

        let length_delimited = Framed::new(stream, LengthDelimitedCodec::new());
        let mut transport: Transport = SerdeFramed::new(length_delimited, Json::default());

        transport.send(request).await?;

        if let Some(resp) = transport.next().await {
            match resp {
                Ok(response) => Ok(response),
                Err(err) => Err(err.into()),
            }
        } else {
            self.log
                .error("Server closed connection without responding");
            Err(ClientError::NoResponse)
        }
    }
}
