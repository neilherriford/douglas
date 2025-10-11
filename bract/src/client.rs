use crate::server::{Request as ServerRequest, Response as ServerResponse};
use crate::{Mount, Service, Version};
use file_system::FileReader;
use futures::{SinkExt, StreamExt};
use log::Logger;
use serde::Serialize;
use std::io::ErrorKind;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Missing token. Has the system been initialized?")]
    MissingToken,
    #[error("Invalid token")]
    InvalidToken,
    #[error("Connection refused.  Is douglas running?")]
    ConnectionRefused,
    #[error("Server closed connection without responding")]
    NoResponse,
    #[error("Server returned an unexpected response")]
    UnexpectedResponse,
    #[error("Error: {0}")]
    Error(String),
}

#[derive(Debug)]
pub struct Credential {
    pub user: String,
    pub group: String,
}

#[derive(Debug, Serialize)]
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
    log: Arc<dyn Logger>,
    file_reader: Arc<dyn FileReader>,
    socket_path: PathBuf,
    token_path: PathBuf,
}

impl Client {
    pub fn new(
        log: Arc<dyn Logger>,
        file_reader: Arc<dyn FileReader>,
        socket_path: &Path,
        token_path: &Path,
    ) -> Self {
        Self {
            log: Arc::clone(&log),
            file_reader,
            token_path: token_path.to_path_buf(),
            socket_path: socket_path.to_path_buf(),
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

    fn create_client_error_from_response(&self, response: ServerResponse) -> ClientError {
        match response {
            ServerResponse::Error(err) => ClientError::Error(err),
            ServerResponse::InvalidToken => ClientError::InvalidToken,
            _ => ClientError::UnexpectedResponse,
        }
    }

    pub async fn active_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::ActiveMountVersion {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
            })
            .await?;

        if let ServerResponse::MountSet {
            name,
            version,
            path,
        } = response
        {
            Ok(Mount {
                name,
                version,
                path,
            })
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn create_credentials(&self, service_name: &str) -> Result<Credential, ClientError> {
        let response = self
            .request(ServerRequest::CreateCredentials {
                token: self.get_token()?,
                service_name: service_name.to_string(),
            })
            .await?;

        if let ServerResponse::CredentialsCreated { user, group } = response {
            Ok(Credential { user, group })
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn create_mount(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::CreateMount {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
            })
            .await?;

        if let ServerResponse::MountSet {
            name,
            version,
            path,
        } = response
        {
            Ok(Mount {
                name,
                version,
                path,
            })
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn create_new_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::CreateNewMountVersion {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
            })
            .await?;

        if let ServerResponse::MountSet {
            name,
            version,
            path,
        } = response
        {
            Ok(Mount {
                name,
                version,
                path,
            })
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn list_mount_versions(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Vec<Version>, ClientError> {
        let response = self
            .request(ServerRequest::ListMountVersions {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
            })
            .await?;

        if let ServerResponse::MountVersionsListed(versions) = response {
            Ok(versions)
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn set_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
        version: Version,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::SetMountVersion {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
                version,
            })
            .await?;

        if let ServerResponse::MountSet {
            name,
            version,
            path,
        } = response
        {
            Ok(Mount {
                name,
                version,
                path,
            })
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn status(&self) -> Result<Status, ClientError> {
        let response = self
            .request(ServerRequest::Status {
                token: self.get_token()?,
            })
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
            Err(self.create_client_error_from_response(response))
        }
    }

    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let response = self
            .request(ServerRequest::Shutdown {
                token: self.get_token()?,
            })
            .await?;

        if let ServerResponse::ShuttingDown = response {
            Ok(())
        } else {
            Err(self.create_client_error_from_response(response))
        }
    }

    async fn request(&self, request: ServerRequest) -> Result<ServerResponse, ClientError> {
        match UnixStream::connect(self.socket_path.as_path()).await {
            Ok(stream) => {
                let length_delimited = Framed::new(stream, LengthDelimitedCodec::new());
                let mut transport: Transport = SerdeFramed::new(length_delimited, Json::default());

                if let Err(err) = transport.send(request).await {
                    return Err(ClientError::Error(format!("{:?}", err)));
                }

                if let Some(resp) = transport.next().await {
                    match resp {
                        Ok(response) => Ok(response),
                        Err(err) => Err(ClientError::Error(format!("{:?}", err))),
                    }
                } else {
                    self.log
                        .error("Server closed connection without responding");
                    Err(ClientError::NoResponse)
                }
            }
            Err(err) => match err.kind() {
                ErrorKind::NotFound => Err(ClientError::MissingToken),
                ErrorKind::ConnectionRefused => Err(ClientError::ConnectionRefused),
                _ => Err(ClientError::Error(format!("{:?}", err))),
            },
        }
    }
}
