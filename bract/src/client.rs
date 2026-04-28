use crate::server::{Request as ServerRequest, Response as ServerResponse};
use crate::{Mount, Service};
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
        todo!();
        // let response = self
        //     .request(ServerRequest::ActiveMountVersion {
        //         token: self.get_token()?,
        //         service_name: service_name.to_string(),
        //         mount_name: mount_name.to_string(),
        //     })
        //     .await?;

        // match response {
        //     ServerResponse::MountVersionListed { version, path } => Ok(Some(Mount {
        //         name: mount_name.into(),
        //         version,
        //         path,
        //     })),
        //     ServerResponse::NoActiveMountVersion => Ok(None),
        //     _ => Err(self.create_unexpected_response("MountVersionListed", response)),
        // }
    }

    pub async fn create_credentials(&self, service_name: &str) -> Result<Credential, ClientError> {
        todo!();
        // let response = self
        //     .request(ServerRequest::CreateCredentials {
        //         token: self.get_token()?,
        //         service_name: service_name.to_string(),
        //     })
        //     .await?;

        // if let ServerResponse::CredentialsCreated {
        //     user,
        //     user_id,
        //     group,
        //     group_id,
        // } = response
        // {
        //     Ok(Credential {
        //         user,
        //         user_id,
        //         group,
        //         group_id,
        //     })
        // } else {
        //     Err(self.create_unexpected_response("CredentialsCreated", response))
        // }
    }

    pub async fn create_mount(
        &self,
        service_name: &str,
        mount_name: &str,
        shared: bool,
    ) -> Result<Mount, ClientError> {
        todo!();
        // let response = self
        //     .request(ServerRequest::CreateMount {
        //         token: self.get_token()?,
        //         service_name: service_name.to_string(),
        //         mount_name: mount_name.to_string(),
        //         shared,
        //     })
        //     .await?;

        // if let ServerResponse::MountSet {
        //     name,
        //     version,
        //     path,
        // } = response
        // {
        //     Ok(Mount {
        //         name,
        //         version,
        //         path,
        //     })
        // } else {
        //     Err(self.create_unexpected_response("MountSet", response))
        // }
    }

    pub async fn create_new_mount_version(
        &self,
        service_name: &str,
        mount_name: &str,
        shared: bool,
    ) -> Result<Mount, ClientError> {
        todo!();
        // let response = self
        //     .request(ServerRequest::CreateNewMountVersion {
        //         token: self.get_token()?,
        //         service_name: service_name.to_string(),
        //         mount_name: mount_name.to_string(),
        //         shared,
        //     })
        //     .await?;

        // if let ServerResponse::MountSet {
        //     name,
        //     version,
        //     path,
        // } = response
        // {
        //     Ok(Mount {
        //         name,
        //         version,
        //         path,
        //     })
        // } else {
        //     Err(self.create_unexpected_response("MountSet", response))
        // }
    }

    // pub async fn list_mount_versions(
    //     &self,
    //     service_name: &str,
    //     mount_name: &str,
    // ) -> Result<Vec<Version>, ClientError> {
    //     todo!();
    //     // let response = self
    //     //     .request(ServerRequest::ListMountVersions {
    //     //         token: self.get_token()?,
    //     //         service_name: service_name.to_string(),
    //     //         mount_name: mount_name.to_string(),
    //     //     })
    //     //     .await?;

    //     // if let ServerResponse::MountVersionsListed(versions) = response {
    //     //     Ok(versions)
    //     // } else {
    //     //     Err(self.create_unexpected_response("MountVersionsListed", response))
    //     // }
    // }

    // pub async fn set_mount_version(
    //     &self,
    //     service_name: &str,
    //     mount_name: &str,
    //     version: Version,
    // ) -> Result<Mount, ClientError> {
    //     todo!();
    //     // let response = self
    //     //     .request(ServerRequest::SetMountVersion {
    //     //         token: self.get_token()?,
    //     //         service_name: service_name.to_string(),
    //     //         mount_name: mount_name.to_string(),
    //     //         version,
    //     //     })
    //     //     .await?;

    //     // if let ServerResponse::MountSet {
    //     //     name,
    //     //     version,
    //     //     path,
    //     // } = response
    //     // {
    //     //     Ok(Mount {
    //     //         name,
    //     //         version,
    //     //         path,
    //     //     })
    //     // } else {
    //     //     Err(self.create_unexpected_response("MountSet", response))
    //     // }
    // }

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
        todo!();
        // match operation {
        //     IoOperation::WriteFile {
        //         relative_path,
        //         contents,
        //     } => {
        //         let response = self
        //             .request(ServerRequest::WriteToMount {
        //                 token: self.get_token()?,
        //                 service_name: service_name.to_string(),
        //                 mount_name: mount_name.to_string(),
        //                 relative_path: relative_path.to_owned(),
        //                 contents: contents.to_string(),
        //             })
        //             .await?;

        //         if let ServerResponse::WroteToMount = response {
        //             Ok(())
        //         } else {
        //             Err(self.create_unexpected_response("WroteToMount", response))
        //         }
        //     }
        // }
    }

    pub async fn shutdown(&self) -> Result<(), ClientError> {
        todo!();
        // let response = self
        //     .request(ServerRequest::Shutdown {
        //         token: self.get_token()?,
        //     })
        //     .await?;

        // if let ServerResponse::ShuttingDown = response {
        //     Ok(())
        // } else {
        //     Err(self.create_unexpected_response("ShuttingDown", response))
        // }
    }

    async fn request(&self, request: ServerRequest) -> Result<ServerResponse, ClientError> {
        self.streaming_request(request, |_| {}).await
    }

    async fn streaming_request(
        &self,
        request: ServerRequest,
        mut on_progress: impl FnMut(&ServerResponse),
    ) -> Result<ServerResponse, ClientError> {
        match UnixStream::connect(self.socket_path.as_path()).await {
            Ok(stream) => {
                let length_delimited = Framed::new(stream, LengthDelimitedCodec::new());
                let mut transport: Transport = SerdeFramed::new(length_delimited, Json::default());

                if let Err(err) = transport.send(request).await {
                    return Err(ClientError::Error(format!("{:?}", err)));
                }

                loop {
                    match transport.next().await {
                        Some(Ok(response)) => {
                            if response.is_terminal() {
                                return Ok(response);
                            }
                            on_progress(&response);
                        }
                        Some(Err(err)) => return Err(ClientError::Error(format!("{:?}", err))),
                        None => {
                            self.log.error("Server closed connection without responding");
                            return Err(ClientError::NoResponse);
                        }
                    }
                }
            }
            Err(err) => match err.kind() {
                ErrorKind::NotFound => Err(ClientError::MissingSocket),
                ErrorKind::ConnectionRefused => Err(ClientError::ConnectionRefused),
                _ => Err(ClientError::Error(format!("{:?}", err))),
            },
        }
    }

    pub(crate) fn create_unexpected_response(
        &self,
        expected: &str,
        response: ServerResponse,
    ) -> ClientError {
        todo!();
        // let received = match response {
        //     ServerResponse::CredentialsCreated { .. } => "CredentialsCreated",
        //     ServerResponse::MountSet { .. } => "MountSet",
        //     ServerResponse::MountVersionListed { .. } => "MountVersionListed",
        //     ServerResponse::NoActiveMountVersion => "NoActiveMountVersion",
        //     ServerResponse::MountVersionsListed(_) => "MountVersionsListed",
        //     ServerResponse::InvalidToken => "InvalidToken",
        //     ServerResponse::Status { .. } => "Status",
        //     ServerResponse::Error(_) => "Error",
        //     ServerResponse::ShuttingDown => "ShuttingDown",
        //     ServerResponse::Stopped => "Stopped",
        //     ServerResponse::WroteToMount => "WroteToMount",
        // };

        // ClientError::UnexpectedResponse {
        //     expected: expected.to_string(),
        //     received: received.to_string(),
        //     details: format!("{response:?}"),
        // }
    }
}

#[cfg(test)]
mod tests {
    mod streaming_request {
        use crate::client::{Client, ClientError};
        use crate::server::{Request as ServerRequest, Response as ServerResponse};
        use file_system::MockFileReader;
        use futures::{SinkExt, StreamExt};
        use log::MockLogger;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::net::UnixListener;
        use tokio_serde::formats::Json;
        use tokio_serde::Framed as SerdeFramed;
        use tokio_util::codec::{Framed, LengthDelimitedCodec};

        static SOCKET_COUNTER: AtomicU32 = AtomicU32::new(0);

        fn unique_socket_path() -> PathBuf {
            PathBuf::from(format!(
                "/tmp/bract_client_test_{}.sock",
                SOCKET_COUNTER.fetch_add(1, Ordering::SeqCst)
            ))
        }

        fn make_client(socket_path: PathBuf) -> Client {
            let mut log = MockLogger::new();
            log.expect_error().returning(|_| ());

            Client::new(
                Box::new(log),
                Box::new(MockFileReader::new()),
                socket_path,
                PathBuf::from("/tmp/unused_token"),
            )
        }

        fn spawn_mock_server(socket_path: &PathBuf, responses: Vec<ServerResponse>) {
            let listener = UnixListener::bind(socket_path).unwrap();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let framed = Framed::new(stream, LengthDelimitedCodec::new());
                let mut transport: SerdeFramed<
                    _,
                    ServerRequest,
                    ServerResponse,
                    Json<ServerRequest, ServerResponse>,
                > = SerdeFramed::new(framed, Json::default());

                // Consume the incoming request.
                let _ = transport.next().await;

                for response in responses {
                    transport.send(response).await.ok();
                }
            });
        }

        // Progress frames invoke the callback; the loop stops at the terminal frame.
        #[tokio::test]
        async fn collects_progress_frames_and_returns_terminal() {
            let socket_path = unique_socket_path();

            spawn_mock_server(
                &socket_path,
                vec![
                    ServerResponse::Progress {
                        index: 0,
                        step: "Create user".into(),
                        message: "User 'svc' created".into(),
                    },
                    ServerResponse::Progress {
                        index: 1,
                        step: "Create group".into(),
                        message: "Group 'svc' created".into(),
                    },
                    ServerResponse::Success,
                ],
            );

            let client = make_client(socket_path.clone());
            let mut progress: Vec<String> = Vec::new();

            let result = client
                .streaming_request(
                    ServerRequest::Status {
                        token: "tok".into(),
                    },
                    |response| {
                        if let ServerResponse::Progress { step, .. } = response {
                            progress.push(step.clone());
                        }
                    },
                )
                .await
                .unwrap();

            std::fs::remove_file(&socket_path).ok();

            assert_eq!(progress, vec!["Create user", "Create group"]);
            assert!(matches!(result, ServerResponse::Success));
        }

        // A handler that sends only a terminal frame (the existing Status/Shutdown
        // path) should work without the callback being invoked at all.
        #[tokio::test]
        async fn single_terminal_response_skips_callback() {
            let socket_path = unique_socket_path();

            spawn_mock_server(&socket_path, vec![ServerResponse::InvalidToken]);

            let client = make_client(socket_path.clone());
            let mut callback_count = 0;

            let result = client
                .streaming_request(
                    ServerRequest::Status {
                        token: "wrong".into(),
                    },
                    |_| callback_count += 1,
                )
                .await
                .unwrap();

            std::fs::remove_file(&socket_path).ok();

            assert_eq!(callback_count, 0);
            assert!(matches!(result, ServerResponse::InvalidToken));
        }

        // The client returns NoResponse when the server closes the connection
        // without sending anything.
        #[tokio::test]
        async fn returns_no_response_when_server_closes_early() {
            let socket_path = unique_socket_path();

            // Server accepts, reads the request, then drops the connection.
            let listener = UnixListener::bind(&socket_path).unwrap();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let framed = Framed::new(stream, LengthDelimitedCodec::new());
                let mut transport: SerdeFramed<
                    _,
                    ServerRequest,
                    ServerResponse,
                    Json<ServerRequest, ServerResponse>,
                > = SerdeFramed::new(framed, Json::default());
                let _ = transport.next().await;
                // drop transport → connection closes
            });

            let client = make_client(socket_path.clone());

            let result = client
                .streaming_request(
                    ServerRequest::Status {
                        token: "tok".into(),
                    },
                    |_| {},
                )
                .await;

            std::fs::remove_file(&socket_path).ok();

            assert!(matches!(result, Err(ClientError::NoResponse)));
        }
    }
}
