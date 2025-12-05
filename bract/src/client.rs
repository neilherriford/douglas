use crate::server::{Request as ServerRequest, Response as ServerResponse};
use crate::{Mount, Service, Version};
use file_system::FileReader;
use futures::{SinkExt, StreamExt};
use log::Logger;
use serde::Serialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
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
    log: Box<dyn Logger>,
    file_reader: Box<dyn FileReader>,
    socket_path: PathBuf,
    token_path: PathBuf,
}

impl Client {
    pub fn new(
        log: Box<dyn Logger>,
        file_reader: Box<dyn FileReader>,
        socket_path: PathBuf,
        token_path: PathBuf,
    ) -> Self {
        Self {
            log,
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

    pub async fn active_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Option<Mount>, ClientError> {
        let response = self
            .request(ServerRequest::ActiveMountVersion {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
            })
            .await?;

        match response {
            ServerResponse::MountVersionListed { version, path } => Ok(Some(Mount {
                name: mount_name.into(),
                version,
                path,
            })),
            ServerResponse::NoActiveMountVersion => Ok(None),
            _ => Err(self.create_unexpected_response("MountVersionListed", response)),
        }
    }

    pub async fn create_credentials(&self, service_name: &str) -> Result<Credential, ClientError> {
        let response = self
            .request(ServerRequest::CreateCredentials {
                token: self.get_token()?,
                service_name: service_name.to_string(),
            })
            .await?;

        if let ServerResponse::CredentialsCreated {
            user,
            user_id,
            group,
            group_id,
        } = response
        {
            Ok(Credential {
                user,
                user_id,
                group,
                group_id,
            })
        } else {
            Err(self.create_unexpected_response("CredentialsCreated", response))
        }
    }

    pub async fn create_mount(
        &self,
        service_name: &str,
        mount_name: &str,
        shared: bool,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::CreateMount {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
                shared,
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
            Err(self.create_unexpected_response("MountSet", response))
        }
    }

    pub async fn create_new_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
        shared: bool,
    ) -> Result<Mount, ClientError> {
        let response = self
            .request(ServerRequest::CreateNewMountVersion {
                token: self.get_token()?,
                service_name: service_name.to_string(),
                mount_name: mount_name.to_string(),
                shared,
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
            Err(self.create_unexpected_response("MountSet", response))
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
            Err(self.create_unexpected_response("MountVersionsListed", response))
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
            Err(self.create_unexpected_response("MountSet", response))
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
            Err(self.create_unexpected_response("Status", response))
        }
    }

    pub async fn mount_io(
        &self,
        service_name: &str,
        mount_name: &str,
        operation: IoOperation<'_>,
    ) -> Result<(), ClientError> {
        match operation {
            IoOperation::WriteFile {
                relative_path,
                contents,
            } => {
                let response = self
                    .request(ServerRequest::WriteToMount {
                        token: self.get_token()?,
                        service_name: service_name.to_string(),
                        mount_name: mount_name.to_string(),
                        relative_path: relative_path.to_owned(),
                        contents: contents.to_string(),
                    })
                    .await?;

                if let ServerResponse::WroteToMount = response {
                    Ok(())
                } else {
                    Err(self.create_unexpected_response("WroteToMount", response))
                }
            }
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
            Err(self.create_unexpected_response("ShuttingDown", response))
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
                ErrorKind::NotFound => Err(ClientError::MissingSocket),
                ErrorKind::ConnectionRefused => Err(ClientError::ConnectionRefused),
                _ => Err(ClientError::Error(format!("{:?}", err))),
            },
        }
    }

    fn create_unexpected_response(&self, expected: &str, response: ServerResponse) -> ClientError {
        let received = match response {
            ServerResponse::CredentialsCreated { .. } => "CredentialsCreated",
            ServerResponse::MountSet { .. } => "MountSet",
            ServerResponse::MountVersionListed { .. } => "MountVersionListed",
            ServerResponse::NoActiveMountVersion => "NoActiveMountVersion",
            ServerResponse::MountVersionsListed(_) => "MountVersionsListed",
            ServerResponse::InvalidToken => "InvalidToken",
            ServerResponse::Status { .. } => "Status",
            ServerResponse::Error(_) => "Error",
            ServerResponse::ShuttingDown => "ShuttingDown",
            ServerResponse::Stopped => "Stopped",
            ServerResponse::WroteToMount => "WroteToMount",
        };

        ClientError::UnexpectedResponse {
            expected: expected.to_string(),
            received: received.to_string(),
            details: format!("{response:?}"),
        }
    }
}
