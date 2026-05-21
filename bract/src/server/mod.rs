use crate::Service;
use crate::server::create_listener::CreateListener;
use crate::server::pipe_reporter::PipeReporter;
use config::{DouglasFolders, constants};
use credentials::{Credentials, CredentialsError};
use file_system::path_to_string;
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links, Listener, Permissions,
    UnixDomainSocket,
};
use futures::{SinkExt, StreamExt};
use log::{BufferedFileReporter, Reporter, ScopeKind, Span, TeeReporter};
use os::{Os, OsError};
use serde::{Deserialize, Serialize};
use shutdown::Shutdown;
use status::Status;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fs::File;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use token_refresher::TokenRefresher;
use tokio::sync::broadcast::{self, Sender};
use tokio::sync::mpsc;
use tokio::time;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

mod create_listener;
mod pipe_reporter;
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
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Response::InvalidToken => f.write_str("Invalid token"),
            Response::Status {
                token_path,
                mount_root,
                services,
            } => f.write_str("Status: tbd"),
            Response::Error(err) => f.write_str(&format!("Error: '{err}'")),
            Response::Success => f.write_str("Success"),
            Response::Plan(items) => todo!(),
            Response::Progress {
                index,
                step,
                message,
            } => f.write_str("Progress"),
        }
    }
}

impl Response {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Response::Progress { .. } | Response::Plan(_))
    }
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
    reporter: Arc<dyn Reporter>,
    status: Status,
    shutdown: Shutdown,
    shutdown_sender: Sender<()>,
}

impl RequestHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        reporter: Arc<dyn Reporter>,
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
        todo!()
    }

    pub async fn handle(&self, request: Request, tx: mpsc::Sender<Response>) {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Handling request",
            ScopeKind::Task,
        );
        let response = match request {
            Request::WriteToMount {
                token,
                service_name,
                mount_name,
                relative_path,
                contents,
            } => {
                todo!()
            }
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
            Request::Status { token } => self.status.perform(&span, token),
            Request::Shutdown { token } => {
                self.shutdown.perform(&span, token, &self.shutdown_sender)
            }
        };
        tx.send(response).await.ok();
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
    request_handler: Arc<RequestHandler>,
    token_refresher: Arc<TokenRefresher>,
    listener_factory: CreateListener,
    credentials: Arc<dyn Credentials>,
    shutdown_sender: Sender<()>,
    reporter: Arc<dyn Reporter>,
}

impl Server {
    pub fn new(
        bootstrap_fd: Option<i32>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        folder: Arc<dyn Folder>,
        links: Arc<dyn Links>,
        os: Arc<dyn Os>,
        permissions: Arc<dyn Permissions>,
        unix_domain_socket: Arc<dyn UnixDomainSocket>,
        credentials: Arc<dyn Credentials>,
        douglas_folders: DouglasFolders,
    ) -> Self {
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        let mut sinks: Vec<Box<dyn Reporter>> = vec![Box::new(BufferedFileReporter::new(
            douglas_folders.log_file("bract"),
        ))];

        if let Some(fd) = bootstrap_fd {
            let file = unsafe { File::from_raw_fd(fd) };
            sinks.push(Box::new(PipeReporter::new(file)));
        }

        let reporter: Arc<dyn Reporter> = Arc::new(TeeReporter::new(sinks));

        Self {
            reporter: reporter.clone(),
            request_handler: Arc::new(RequestHandler::new(
                reporter,
                Arc::clone(&folder),
                file_reader,
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&links),
                Arc::clone(&credentials),
                Arc::clone(&permissions),
                &douglas_folders.transients,
                &douglas_folders.application_mounts,
                shutdown_sender.clone(),
            )),

            token_refresher: Arc::new(TokenRefresher::new(
                &douglas_folders.transients,
                Arc::clone(&permissions),
                file_writer,
                os,
            )),
            listener_factory: CreateListener::new(
                &douglas_folders.socket_file("bract"),
                Arc::clone(&file_deleter),
                Arc::clone(&permissions),
                Arc::clone(&unix_domain_socket),
            ),
            credentials: Arc::clone(&credentials),
            shutdown_sender,
        }
    }

    pub fn start(&self) -> Result<(), ServerError> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting server",
            ScopeKind::Group,
        );

        self.token_refresher.refresh(&span);

        let scoped_reporter = span.create_scoped_reporter();
        let rt = tokio::runtime::Runtime::new().inspect_err(|e| {
            scoped_reporter.message(log::Level::Warn, &format!("Runtime error: {e}"))
        })?;
        rt.block_on(async {
            let listener = self
                .listener_factory
                .create(&span)
                .inspect_err(|e| scoped_reporter.message(log::Level::Warn, &format!("Runtime error: {e}")))?;

            let token_refresh = Self::token_refresh_task(
                &span,
                Arc::clone(&self.token_refresher),
                self.shutdown_sender.subscribe(),
            );
            let request_handler =
                Self::request_handler_task(&span, listener, Arc::clone(&self.request_handler));

            tokio::select! {
                r = token_refresh => r.inspect_err(|e| scoped_reporter.message(log::Level::Warn, &format!("Runtime error: {e}")))?,
                r = request_handler => r.inspect_err(|e| scoped_reporter.message(log::Level::Warn, &format!("Runtime error: {e}")))?,
            }

            scoped_reporter.message(log::Level::Info, "Shutting down");
            scoped_reporter.finish(log::Outcome::Ok);
            Ok(())
        })
    }

    async fn request_handler_task(
        span: &Span,
        listener: Box<dyn Listener + Sync + Send>,
        handler: Arc<RequestHandler>,
    ) -> Result<(), ServerError> {
        let child_span = span.create_child("Listening", ScopeKind::Phase);

        loop {
            let (socket, _) = listener.accept().await?;
            let handler = Arc::clone(&handler);
            let child_span = child_span.clone();
            let request_span = child_span.create_child("Received request", ScopeKind::Group);

            tokio::spawn(async move {
                let length_delimited = Framed::new(socket, LengthDelimitedCodec::new());
                let mut transport =
                    SerdeFramed::new(length_delimited, Json::<Request, Response>::default());

                match transport.next().await {
                    Some(Ok(request)) => {
                        let reporter = request_span.create_scoped_reporter();
                        reporter.message(log::Level::Info, &request.to_string());

                        let (tx, mut rx) = mpsc::channel(32);
                        let handler = Arc::clone(&handler);
                        tokio::spawn(async move {
                            handler.handle(request, tx).await;
                        });

                        while let Some(response) = rx.recv().await {
                            if transport.send(response).await.is_err() {
                                break;
                            }
                        }

                        reporter.finish(log::Outcome::Ok);
                    }
                    None => {
                        let reporter = request_span.create_scoped_reporter();
                        reporter.message(log::Level::Warn, "Invalid request");
                        let _ = transport
                            .send(Response::Error("Invalid request".to_string()))
                            .await;
                    }
                    Some(Err(err)) => {
                        let reporter = request_span.create_scoped_reporter();
                        let message = format!("Invalid request: {:?}", err);
                        reporter.message(log::Level::Warn, &message);
                        let _ = transport.send(Response::Error(message)).await;
                    }
                }
            });
        }
    }

    async fn token_refresh_task(
        span: &Span,
        token_refresher: Arc<TokenRefresher>,
        mut shutdown_receiver: broadcast::Receiver<()>,
    ) -> Result<(), ServerError> {
        let child_span = span.create_child("Starting token refresh", ScopeKind::Task);
        let log = child_span.create_scoped_reporter();
        loop {
            tokio::select! {
                _ = time::sleep(Duration::from_secs(FIVE_MINUTES)) => {
                    token_refresher.refresh(&child_span);
                },
                _ = shutdown_receiver.recv() => {
                    break;
                }
            }
        }
        log.finish(log::Outcome::Ok);
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
