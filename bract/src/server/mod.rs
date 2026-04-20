use crate::Service;
use crate::server::create_listener::CreateListener;
use config::{SystemPaths, constants};
use credentials::{Credentials, CredentialsError};
use file_system::path_to_string;
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links, Listener, Permissions,
    UnixDomainSocket,
};
use futures::{SinkExt, StreamExt};
use log::Logger;
use mount_io::MountIo;
use mount_writer::MountWriter;
use os::{Os, OsError};
use serde::{Deserialize, Serialize};
use shutdown::Shutdown;
use status::Status;
use std::collections::HashSet;
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

#[macro_use]
mod macros;
mod create_listener;
mod create_service_mounts;
mod mount_io;
mod mount_writer;
mod service_account_manager;
mod service_mount_manager;
mod shutdown;
mod status;
mod token_refresher;
mod token_validator;

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct Name {
    pub display: String,
    pub system: String,
}

impl Name {
    fn from_non_truncated(non_truncated: &str) -> Self {
        Self {
            display: non_truncated.to_string(),
            system: non_truncated.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Credential {
    pub id: u32,
    pub display_name: String,
    pub system_name: String,
}
impl Credential {
    fn from_name(id: u32, name: Name) -> Self {
        Self {
            id,
            display_name: name.display,
            system_name: name.system,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CreatedMountDefinition {
    pub name: String,
    pub shared: Shared,
    pub share_group: Option<Credential>,
    pub ephemeral: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "data")]
pub(crate) enum Response {
    InvalidToken,
    Status {
        token_path: PathBuf,
        mount_root: PathBuf,
        services: Vec<Service>,
    },
    Error(String),
    Success,
    Plan(Vec<String>),
    Progress {
        index: usize,
        step: String,
        message: String,
    },
    CreatedServiceMounts {
        token: String,
        service_name: String,
        mounts: HashSet<CreatedMountDefinition>,
        user: Credential,
        group: Credential,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
pub enum Shared {
    No,
    WithServices(Vec<String>),
}

impl std::fmt::Display for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shared::No => f.write_str("No"),
            Shared::WithServices(names) => f.write_str(&format!(
                "WithServices:[{}]",
                names.iter().fold(String::new(), |mut acc, name| {
                    if !acc.is_empty() {
                        acc.push_str(", ");
                    }
                    acc.push_str(name.as_str());
                    acc
                })
            )),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
pub struct MountDefinition {
    pub name: String,
    pub shared: Shared,
    pub ephemeral: bool,
}

impl std::fmt::Display for MountDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "Name: {} shared: {} ephemeral: {}",
            self.name, self.shared, self.ephemeral
        ))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub(crate) enum Request {
    CreateServiceMounts {
        token: String,
        service_name: String,
        mounts: HashSet<MountDefinition>,
    },
    SetupEphemeralMounts {
        token: String,
        service_name: String,
    },
    TearDownEphemeralMounts {
        token: String,
        service_name: String,
    },
    WriteToMount {
        token: String,
        service_name: String,
        mount_name: String,
        relative_path: PathBuf,
        contents: String,
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
        match self {
            Request::CreateServiceMounts {
                service_name,
                mounts,
                ..
            } => {
                let pretty_mounts = mounts.iter().fold(String::new(), |mut acc, definition| {
                    if !acc.is_empty() {
                        acc.push_str(", ");
                    }
                    acc.push_str(&format!("({}: shared: {}, ephemeral: {})",definition.name, definition.shared,definition.ephemeral));
                    acc
                });

                f.write_str(&format!("CreateServiceMounts service_name: {service_name} mounts: [{pretty_mounts}]"))
            },
            Request::SetupEphemeralMounts { service_name, .. } => f.write_str(&format!("SetupEphemeralMounts service_name: {service_name}")),
            Request::TearDownEphemeralMounts { service_name, .. } => f.write_str(&format!("TearDownEphemeralMounts service_name: {service_name}")),
            Request::WriteToMount {
                service_name,
                mount_name,
                relative_path,
                ..
            } => f.write_str(&format!("WriteToMount service_name: {service_name} mount_name: {mount_name}, relative_path: {}", path_to_string(relative_path))),
            Request::Status { .. } => f.write_str("Status"),
            Request::Shutdown { .. } => f.write_str("Shutdown"),
        }
    }
}

struct RequestHandler {
    status: Status,
    shutdown: Shutdown,
    mount_writer: MountWriter,
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

        let mount_io = Arc::new(MountIo::new(
            Arc::clone(&folder),
            file_writer,
            Arc::clone(&permissions),
        ));

        Self {
            mount_writer: MountWriter::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&mount_io),
            ),
            status: Status::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                token_path,
                mount_root,
            ),
            shutdown: Shutdown::new(Arc::clone(&log), Arc::clone(&token_validator)),
            shutdown_sender,
        }
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
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
            Request::CreateServiceMounts {
                token,
                service_name,
                mounts,
            } => todo!(),
            Request::SetupEphemeralMounts {
                token,
                service_name,
            } => todo!(),
            Request::TearDownEphemeralMounts {
                token,
                service_name,
            } => todo!(),
            Request::Status { token } => self.status.perform(token),
            Request::Shutdown { token } => self.shutdown.perform(token, &self.shutdown_sender),
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

    fn assert_initalized(&self) -> Result<(), ServerError> {
        if self.credentials.group_exists(constants::DOUGLAS_APP_GROUP) {
            Ok(())
        } else {
            Err(ServerError::NotInitialized)
        }
    }
}
