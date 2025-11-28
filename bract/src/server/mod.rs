use crate::{Service, Version};
use active_mount_version::ActiveMountVersion;
use config::{SystemPaths, constants};
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
use mount_io::MountIo;
use mount_path_factory::MountPathFactory;
use mount_writer::MountWriter;
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
mod mount_io;
mod mount_path_factory;
mod mount_writer;
mod set_mount_version;
mod shutdown;
mod status;
mod token_refresher;
mod token_validator;
mod version_manager;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "data")]
pub(crate) enum Response {
    CredentialsCreated {
        user: String,
        user_id: u32,
        group: String,
        group_id: u32,
    },
    MountSet {
        name: String,
        version: Version,
        path: PathBuf,
    },
    NoActiveMountVersion,
    MountVersionListed {
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
    WroteToMount,
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
    WriteToMount {
        token: String,
        service_name: String,
        mount_name: String,
        relative_path: PathBuf,
        contents: String,
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
                "ActiveMountVersion service_name: '{service_name}', mount_name: '{mount_name}'"
            ),
            Request::CreateCredentials {
                token: _,
                service_name,
            } => format!("CreateCredentials service_name: '{service_name}'"),
            Request::CreateMount {
                token: _,
                service_name,
                mount_name,
            } => format!("CreateMount service_name: '{service_name}', mount_name: '{mount_name}'",),
            Request::CreateNewMountVersion {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "CreateNewMountVersion service_name: '{service_name}', mount_name: '{mount_name}'",
            ),
            Request::ListMountVersions {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "ListMountVersions service_name: '{service_name}', mount_name: '{mount_name}'",
            ),
            Request::SetMountVersion {
                token: _,
                service_name,
                mount_name,
                version,
            } => format!(
                "SetMountVersion service_name: '{service_name}', mount_name: '{mount_name}', version: '{version}'",
            ),
            Request::Status { token: _ } => "Status".to_string(),
            Request::Shutdown { token: _ } => "Shutdown".to_string(),
            Request::WriteToMount {
                token: _,
                service_name,
                mount_name,
                relative_path,
                contents,
            } => format!(
                "WriteToMount service_name: '{service_name}', mount_name: '{mount_name}', relative_path: '{}', size: '{}' ",
                relative_path.to_str().unwrap_or("unknown"),
                contents.len()
            ),
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
    mount_writer: MountWriter,
    set_mount_version: SetMountVersion,
    status: Status,
    shutdown: Shutdown,
    shutdown_sender: Sender<()>,
}

impl RequestHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        log: Arc<dyn Logger + Send + Sync>,
        folder: Arc<dyn Folder>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        links: Arc<dyn Links>,
        credentials: Arc<dyn Credentials>,
        permissions: Arc<dyn Permissions>,
        token_path: &Path,
        mount_root: &Path,
        shutdown_sender: Sender<()>,
    ) -> Self {
        let token_validator = Arc::new(TokenValidator::new(
            Arc::clone(&log),
            file_reader,
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
            file_deleter,
            Arc::clone(&permissions),
            Arc::clone(&credentials),
        ));

        let mount_io = Arc::new(MountIo::new(
            Arc::clone(&mount_path_factory),
            Arc::clone(&folder),
            file_writer,
            Arc::clone(&permissions),
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
            mount_writer: MountWriter::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&mount_io),
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
            Request::WriteToMount {
                token,
                service_name,
                mount_name,
                relative_path,
                contents,
            } => self.mount_writer.perform(
                token,
                &service_name,
                &mount_name,
                relative_path,
                &contents,
            ),
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
    credentials: Arc<dyn Credentials>,
    shutdown_sender: Sender<()>,
}

impl Server {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        folder: Arc<dyn Folder>,
        links: Arc<dyn Links>,
        os: Arc<dyn Os>,
        permissions: Arc<dyn Permissions>,
        unix_domain_socket: Arc<dyn UnixDomainSocket>,
        credentials: Arc<dyn Credentials>,
        system_paths: Arc<dyn SystemPaths>,
    ) -> Self {
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        Self {
            log: Arc::clone(&log),
            request_handler: Arc::new(RequestHandler::new(
                Arc::clone(&log),
                Arc::clone(&folder),
                file_reader,
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&links),
                Arc::clone(&credentials),
                Arc::clone(&permissions),
                &system_paths.token_path(),
                &system_paths.mount_root(),
                shutdown_sender.clone(),
            )),

            token_refresher: Arc::new(TokenRefresher::new(
                Arc::clone(&log),
                &system_paths.token_path(),
                Arc::clone(&permissions),
                file_writer,
                os,
            )),
            listener_factory: CreateListener::new(
                Arc::clone(&log),
                &system_paths.douglas_socket_path("bract"),
                Arc::clone(&file_deleter),
                Arc::clone(&permissions),
                Arc::clone(&unix_domain_socket),
            ),
            credentials: Arc::clone(&credentials),
            shutdown_sender,
        }
    }

    pub fn start(&self) -> Result<(), ServerError> {
        self.log.info("Starting server…");
        self.log.info("Verifying permissions");
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
                Arc::clone(&self.log),
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
        listener: Box<dyn Listener + Sync + Send>,
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
        log: Arc<dyn Logger + Sync + Send>,
        mut shutdown_rceiver: broadcast::Receiver<()>,
    ) -> Result<(), ServerError> {
        log.info("Starting refresh task…");
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

    // TODO: tone it down
    fn assert_initalized(&self) -> Result<(), ServerError> {
        if self.credentials.user_exists(constants::RADICLE_USER)
            && self.credentials.group_exists(constants::RADICLE_GROUP)
            && self.credentials.group_exists(constants::DOUGLAS_GROUP)
        {
            Ok(())
        } else {
            Err(ServerError::NotInitialized)
        }
    }
}
