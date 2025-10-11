use crate::{Service, Version};
use active_mount_version::ActiveMountVersion;
use create_credentials::CreateCredentials;
use create_listener::CreateListener;
use create_mount::CreateMount;
use create_new_mount_version::CreateNewMountVersion;
use credentials::{Credentials, CredentialsError};
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links, Listener, Permissions,
    UnixDomainSocket,
};
use futures::{SinkExt, StreamExt};
use list_mount_versions::ListMountVersions;
use log::Logger;
use mockall::automock;
use mount_path_factory::MountPathFactory;
use os::{Os, OsError};
use serde::{Deserialize, Serialize};
use set_mount_version::SetMountVersion;
use shutdown::Shutdown;
use status::Status;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use token_refresher::TokenRefresher;
use token_validator::TokenValidator;
use tokio::sync::broadcast::{self, Sender};
use tokio::time;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use version_manager::VersionManager;

#[macro_use]
mod macros;
mod active_mount_version;
mod create_credentials;
mod create_listener;
mod create_mount;
mod create_new_mount_version;
mod list_mount_versions;
mod mount_path_factory;
mod set_mount_version;
mod shutdown;
mod status;
mod token_refresher;
mod token_validator;
mod version_manager;

#[automock]
pub(super) trait ClientErrorDisplay {
    fn to_client_string(&self) -> String;
}

impl ClientErrorDisplay for FileSystemError {
    fn to_client_string(&self) -> String {
        "Could not create mount".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "data")]
pub(crate) enum Response {
    CredentialsCreated {
        user: String,
        group: String,
    },
    MountSet {
        name: String,
        version: Version,
        path: PathBuf,
    },
    MountVersionsListed(Vec<Version>),
    InvalidToken,
    Status {
        token_path: PathBuf,
        mount_root: PathBuf,
        services: Vec<Service>,
    },
    Error(String),
    ShuttingDown,
    Stopped,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub(crate) enum Request {
    ActiveMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
    },
    CreateCredentials {
        token: String,
        service_name: String,
    },
    CreateMount {
        token: String,
        service_name: String,
        mount_name: String,
    },
    CreateNewMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
    },
    ListMountVersions {
        token: String,
        service_name: String,
        mount_name: String,
    },
    SetMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
        version: Version,
    },
    Status {
        token: String,
    },
    Shutdown {
        token: String,
    },
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Request::ActiveMountVersion {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "ActiveMountVersion service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::CreateCredentials {
                token: _,
                service_name,
            } => format!("CreateCredentials service_name: '{}'", service_name),
            Request::CreateMount {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "CreateMount service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::CreateNewMountVersion {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "CreateNewMountVersion service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::ListMountVersions {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "ListMountVersions service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::SetMountVersion {
                token: _,
                service_name,
                mount_name,
                version,
            } => format!(
                "SetMountVersion service_name: '{}', mount_name: '{}', version: '{}'",
                service_name, mount_name, version
            ),
            Request::Status { token: _ } => "Status".to_string(),
            Request::Shutdown { token: _ } => "Shutdown".to_string(),
        };

        write!(f, "{}", value)
    }
}

struct RequestHandler {
    active_mount_version: ActiveMountVersion,
    create_credentials: CreateCredentials,
    create_mount: CreateMount,
    create_new_mount_version: CreateNewMountVersion,
    list_mount_versions: ListMountVersions,
    set_mount_version: SetMountVersion,
    status: Status,
    shutdown: Shutdown,
    shutdown_sender: Sender<()>,
}

impl RequestHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        log: Arc<dyn Logger + Send + Sync>,
        folder: Arc<dyn Folder + Send + Sync>,
        file_reader: Arc<dyn FileReader + Send + Sync>,
        file_deleter: Arc<dyn FileDeleter + Send + Sync>,
        links: Arc<dyn Links + Send + Sync>,
        credentials: Arc<dyn Credentials + Send + Sync>,
        permissions: Arc<dyn Permissions + Send + Sync>,
        token_path: &Path,
        mount_root: &Path,
        marker_group_name: &str,
        shutdown_sender: Sender<()>,
    ) -> Self {
        let token_validator = Arc::new(TokenValidator::new(
            Arc::clone(&log),
            Arc::clone(&file_reader),
            token_path,
        ));

        let mount_path_factory = Arc::new(MountPathFactory::new(
            mount_root,
            Arc::clone(&folder),
            Arc::clone(&links),
        ));

        let version_manager = Arc::new(VersionManager::new(
            Arc::clone(&mount_path_factory),
            Arc::clone(&folder),
            Arc::clone(&links),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&credentials),
        ));

        Self {
            active_mount_version: ActiveMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&mount_path_factory),
            ),
            create_credentials: CreateCredentials::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&credentials),
                marker_group_name,
            ),
            create_mount: CreateMount::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            create_new_mount_version: CreateNewMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            list_mount_versions: ListMountVersions::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            set_mount_version: SetMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            status: Status::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
                token_path,
                mount_root,
            ),
            shutdown: Shutdown::new(Arc::clone(&log), Arc::clone(&token_validator)),
            shutdown_sender,
        }
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::ActiveMountVersion {
                token,
                service_name,
                mount_name,
            } => self
                .active_mount_version
                .perform(token, service_name, mount_name),
            Request::CreateCredentials {
                token,
                service_name,
            } => self.create_credentials.create(token, service_name),
            Request::CreateMount {
                token,
                service_name,
                mount_name,
            } => self.create_mount.create(token, service_name, mount_name),
            Request::CreateNewMountVersion {
                token,
                service_name,
                mount_name,
            } => self
                .create_new_mount_version
                .create(token, service_name, mount_name),
            Request::ListMountVersions {
                token,
                service_name,
                mount_name,
            } => self
                .list_mount_versions
                .list(token, service_name, mount_name),
            Request::SetMountVersion {
                token,
                service_name,
                mount_name,
                version,
            } => self
                .set_mount_version
                .perform(token, service_name, mount_name, version),
            Request::Status { token } => self.status.perform(token),
            Request::Shutdown { token } => {
                self.shutdown.perform(token, self.shutdown_sender.clone())
            }
        }
    }
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Must be root")]
    NotRootError,
    #[error("Not initialized, must run init before starting Bract")]
    NotInitialized,
    #[error("OS error {0}")]
    OsError(#[from] OsError),
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
}

pub(crate) static FIVE_MINUTES: u64 = 5 * 60;

pub struct Server {
    log: Arc<dyn Logger + Sync + Send>,
    request_handler: Arc<RequestHandler>,
    token_refresher: Arc<TokenRefresher>,
    listener_factory: CreateListener,
    credentials: Arc<dyn Credentials + Sync + Send>,
    service_user_name: String,
    service_group_name: String,
    marker_group_name: String,
    shutdown_sender: Sender<()>,
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        file_reader: Arc<dyn FileReader + Sync + Send>,
        file_writer: Arc<dyn FileWriter + Sync + Send>,
        file_deleter: Arc<dyn FileDeleter + Sync + Send>,
        folder: Arc<dyn Folder + Sync + Send>,
        links: Arc<dyn Links + Sync + Send>,
        os: Arc<dyn Os>,
        permissions: Arc<dyn Permissions + Sync + Send>,
        unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
        credentials: Arc<dyn Credentials + Sync + Send>,
        token_path: &Path,
        socket_path: &Path,
        mount_root: &Path,
        service_user_name: &str,
        service_group_name: &str,
        marker_group_name: &str,
    ) -> Self {
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        Self {
            log: Arc::clone(&log),
            request_handler: Arc::new(RequestHandler::new(
                Arc::clone(&log),
                Arc::clone(&folder),
                Arc::clone(&file_reader),
                Arc::clone(&file_deleter),
                Arc::clone(&links),
                Arc::clone(&credentials),
                Arc::clone(&permissions),
                token_path,
                mount_root,
                marker_group_name,
                shutdown_sender.clone(),
            )),
            token_refresher: Arc::new(TokenRefresher::new(
                Arc::clone(&log),
                token_path,
                Arc::clone(&permissions),
                file_writer,
                Arc::clone(&os),
                service_group_name,
            )),
            listener_factory: CreateListener::new(
                Arc::clone(&log),
                socket_path,
                Arc::clone(&file_deleter),
                Arc::clone(&permissions),
                unix_domain_socket,
                service_group_name,
            ),
            credentials: Arc::clone(&credentials),
            service_user_name: service_user_name.to_string(),
            service_group_name: service_group_name.to_string(),
            marker_group_name: marker_group_name.to_string(),
            shutdown_sender,
        }
    }

    pub fn start(&self) -> Result<(), ServerError> {
        self.log.info("Starting server…");
        self.log.info("Verifying permissions");
        self.assert_root()?;

        self.log.info("Verifying initialization complete");
        self.assert_initalized()?;
        self.log.info("Refreshing token");
        self.token_refresher.refresh();

        let log = Arc::clone(&self.log);

        let rt = tokio::runtime::Runtime::new()
            .inspect_err(|e| self.log.error(&format!("Runtime error: {e}")))?;
        rt.block_on(async {
            log.info("Creating listner");
            let listener = self
                .listener_factory
                .create()
                .inspect_err(|e| self.log.error(&format!("{e}")))?;

            let token_refresh = Self::token_refresh_task(
                Arc::clone(&self.token_refresher),
                self.shutdown_sender.subscribe(),
            );
            let request_handler = Self::request_handler_task(
                listener,
                Arc::clone(&log),
                Arc::clone(&self.request_handler),
            );
            log.info("Started!");

            tokio::select! {
                r = token_refresh => r.inspect_err(|e| self.log.error(&format!("{e}")))?,
                r = request_handler => r.inspect_err(|e| self.log.error(&format!("{e}")))?,
            }

            log.info("Shutting down");

            Ok(())
        })
    }

    async fn request_handler_task(
        listener: Box<dyn Listener>,
        log: Arc<dyn Logger + Sync + Send>,
        handler: Arc<RequestHandler>,
    ) -> Result<(), ServerError> {
        log.info("Listening…");

        loop {
            let (socket, _) = listener.accept().await?;
            let log = Arc::clone(&log);
            let handler = Arc::clone(&handler);

            tokio::spawn(async move {
                let length_delimited = Framed::new(socket, LengthDelimitedCodec::new());
                let mut transport =
                    SerdeFramed::new(length_delimited, Json::<Request, Response>::default());

                match transport.next().await {
                    Some(Ok(request)) => {
                        log.info(&format!("Received request {}", request));
                        let response = handler.handle(request.clone());
                        let _ = transport.send(response).await;
                        log.info("Completed request");
                    }
                    None => {
                        log.warn("Invalid request");
                        let _ = transport
                            .send(Response::Error("Invalid request".to_string()))
                            .await;
                    }
                    Some(Err(err)) => {
                        let message = format!("Invalid request: {:?}", err);
                        log.error(&message);
                        let _ = transport.send(Response::Error(message.to_string())).await;
                    }
                }
            });
        }
    }

    async fn token_refresh_task(
        token_refresher: Arc<TokenRefresher>,
        mut shutdown_rceiver: broadcast::Receiver<()>,
    ) -> Result<(), ServerError> {
        loop {
            tokio::select! {
                _ = time::sleep(Duration::from_secs(FIVE_MINUTES)) => {
                    token_refresher.refresh();
                },
                _ = shutdown_rceiver.recv() => {
                    break;
                }
            }
        }
        Ok(())
    }

    fn assert_root(&self) -> Result<(), ServerError> {
        if self.credentials.is_root() {
            Ok(())
        } else {
            self.log.error("Not root!");
            Err(ServerError::NotRootError)
        }
    }

    fn assert_initalized(&self) -> Result<(), ServerError> {
        if self.credentials.user_exists(&self.service_user_name)
            && self.credentials.group_exists(&self.service_group_name)
            && self.credentials.group_exists(&self.marker_group_name)
        {
            Ok(())
        } else {
            Err(ServerError::NotInitialized)
        }
    }
}
