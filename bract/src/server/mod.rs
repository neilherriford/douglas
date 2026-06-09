use crate::Service;
use crate::server::create_listener::CreateListener;
use crate::server::pipe_reporter::PipeReporter;
use crate::server::token_validator::TokenValidator;
use blueprint::{Command, CommandExecutor, JournalingExecutor, RunningStatus};
use config::constants::DOUGLAS_ADMIN_GROUP;
use config::{DouglasFolders, constants};
use credentials::{Credentials, CredentialsError, create_credentials};
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileReader, FileSystemError, FileWriter, Folder,
    Links, Listener, Modes, Permissions, UnixDomainSocket, UnixFileDeleter, UnixFileReader,
    UnixFileWriter, UnixFolder, UnixLinks, UnixPermissions, path_to_string,
};
use futures::{SinkExt, StreamExt};
use log::{BufferedFileReporter, Level, Outcome, Reporter, ScopeKind, Span, TeeReporter};
use os::{EnvironmentVariableReader, Os, OsError, Unix, UnixEnvironmentVariableReader};
use serde::{Deserialize, Serialize};
use shutdown::Shutdown;
use status::Status;
use std::{
    collections::HashSet,
    env::VarError,
    fmt::Debug,
    num::ParseIntError,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
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
        let token_validator = Arc::new(TokenValidator::new(file_reader, token_path));

        Self {
            reporter,
            status: Status::new(Arc::clone(&token_validator), token_path, mount_root),
            shutdown: Shutdown::new(Arc::clone(&token_validator)),
            shutdown_sender,
        }
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
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Docker must be running")]
    MustHaveRunningDocker,
    #[error("Invalid PID: {0}")]
    InvalidPid(#[from] ParseIntError),
    #[error("Failed to bootstrap")]
    FailedBoostrap(Vec<String>),
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
    pub async fn build(reporting_fd: Option<i32>) -> Result<Self, ServerError> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = Box::new(UnixFolder::new());
        let file_reader = Box::new(UnixFileReader::new());
        let file_writer = Box::new(UnixFileWriter::new());
        let file_deleter = Box::new(UnixFileDeleter::new());
        let links = Box::new(UnixLinks::new());
        let unix_domain_socket = Box::new(UnixDomainSocket::new());
        let permissions = Box::new(UnixPermissions::new());
        let environment_variable_reader = Box::new(UnixEnvironmentVariableReader::new());
        let douglas_folders = DouglasFolders::new();

        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        let boot_reporter: Arc<dyn Reporter> = {
            let mut sinks: Vec<Box<dyn Reporter>> = vec![Box::new(BufferedFileReporter::new(
                douglas_folders.log_file("bract"),
            ))];
            if let Some(fd) = reporting_fd {
                sinks.push(Box::new(unsafe { PipeReporter::from_raw_fd(fd) }));
            }
            Arc::new(TeeReporter::new(sinks))
        };

        let server_reporter: Arc<dyn Reporter> =
            Arc::new(BufferedFileReporter::new(douglas_folders.log_file("bract")));

        let guard = Span::new(
            Arc::clone(&boot_reporter),
            "Starting douglas-bract system…",
            log::ScopeKind::Group,
        )
        .start_guard();

        let mut docker_ping: Box<dyn docker::Ping> =
            match docker::UdsPing::build_with_default_socket_path().await {
                Ok(client) => Box::new(client),
                Err(err) => {
                    let message = format!("Failed to connect to Docker: {err}");
                    eprintln!("{message}");
                    guard.span().message(Level::Warn, &message);
                    return guard.finish(Err(ServerError::MustHaveRunningDocker));
                }
            };

        bootstrap(
            Arc::clone(&boot_reporter),
            &*credentials,
            &*folder,
            &*permissions,
            &*environment_variable_reader,
            &mut *docker_ping,
            &douglas_folders,
        )
        .await?;

        let file_writer: Arc<dyn FileWriter> = Arc::from(*file_writer);
        let file_deleter: Arc<dyn FileDeleter> = Arc::from(*file_deleter);
        let credentials: Arc<dyn Credentials> = Arc::from(credentials);
        let permissions: Arc<dyn Permissions> = Arc::from(*permissions);

        let result = Self::new(
            server_reporter,
            file_reader,
            file_writer,
            file_deleter,
            folder,
            links,
            credentials,
            permissions,
            shutdown_sender,
            unix_domain_socket,
            os,
            &douglas_folders,
        );

        guard.finish_with_outcome(Outcome::Ok);
        drop(guard);
        drop(boot_reporter);

        Ok(result)
    }

    fn new(
        reporter: Arc<dyn Reporter>,
        file_reader: Box<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        folder: Box<dyn Folder>,
        links: Box<dyn Links>,
        credentials: Arc<dyn Credentials>,
        permissions: Arc<dyn Permissions>,
        shutdown_sender: Sender<()>,
        unix_domain_socket: Box<dyn BindableUnixDomainSocketFile>,
        os: Arc<dyn Os>,
        douglas_folders: &DouglasFolders,
    ) -> Server {
        Self {
            reporter: reporter.clone(),
            request_handler: Arc::new(RequestHandler::new(
                reporter,
                Arc::from(folder),
                Arc::from(file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::from(links),
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
                Arc::from(unix_domain_socket),
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

async fn bootstrap(
    reporter: Arc<dyn Reporter>,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    environment_variable_reader: &dyn EnvironmentVariableReader,
    docker_ping: &mut dyn docker::Ping,
    douglas_folders: &DouglasFolders,
) -> Result<(), ServerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Bootstrapping douglas-bract system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver::new(
            credentials,
            folder,
            permissions,
            environment_variable_reader,
            docker_ping,
        );
        state_observer
            .discover(guard.span(), douglas_folders)
            .await?
    };

    if !state.is_root {
        guard.span().message(Level::Warn, "Must be root");
        return guard.finish(Err(ServerError::MustBeRoot));
    }

    if state.docker_running_status == RunningStatus::NotRunning {
        guard.span().message(Level::Warn, "Docker must be running");
        return guard.finish(Err(ServerError::MustHaveRunningDocker));
    }

    let plan = match create_plan(state) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return guard.finish(Err(err));
        }
    };

    guard
        .span()
        .plan_hint(plan.iter().map(std::string::ToString::to_string).collect());

    let mut executor = JournalingExecutor::new();
    let execution_result = {
        let mut context = Context {
            credentials,
            folder,
            permissions,
        };
        executor.run(
            &guard
                .span()
                .create_child("Executing plan", ScopeKind::Phase),
            &mut context,
            plan,
        )
    };

    match execution_result {
        blueprint::ExecutionResult::Success => guard.finish(Ok(())),
        blueprint::ExecutionResult::Failed {
            failed_at_step,
            failed_at_step_name,
            perform_error,
            rollback_errors,
        } => {
            guard.span().message(
                Level::Warn,
                &format!(
                    "Failed at step {failed_at_step}. {failed_at_step_name}: '{perform_error}'"
                ),
            );
            if !rollback_errors.is_empty() {
                guard.span().message(
                    Level::Warn,
                    "Additionally, ran into these errors while rolling back",
                );
                for error in rollback_errors {
                    guard.span().message(Level::Warn, &error.to_string());
                }
            }
            guard.finish(Err(ServerError::FailedBoostrap(Vec::new())))
        }
    }
}

struct FolderOwnershipRequirement {
    path: PathBuf,
    owning_user_name: String,
    owning_group_name: String,
}

struct FolderModeRequirement {
    path: PathBuf,
    mode: Modes,
}

struct GroupMembershipRequirement {
    group_name: String,
    user_name: String,
}

struct Context<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
    permissions: &'a dyn Permissions,
}

#[derive(Default)]
struct State {
    is_root: bool,
    bract_running_status: RunningStatus,
    docker_running_status: RunningStatus,
    groups_missing: Vec<String>,
    group_members_missing: Vec<GroupMembershipRequirement>,
    folders_missing: Vec<PathBuf>,
    folders_missing_ownership: Vec<FolderOwnershipRequirement>,
    folders_missing_mode: Vec<FolderModeRequirement>,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
    permissions: &'a dyn Permissions,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
    docker_ping: &'a mut dyn docker::Ping,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        folder: &'a dyn Folder,
        permissions: &'a dyn Permissions,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
        docker_ping: &'a mut dyn docker::Ping,
    ) -> Self {
        Self {
            credentials,
            folder,
            permissions,
            environment_variable_reader,
            docker_ping,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, ServerError> {
        let guard = span
            .create_child("Bract Start: Discover", ScopeKind::Phase)
            .start_guard();

        let mut result = State {
            bract_running_status: self.check_socket(guard.span(), "bract.sock", douglas_folders),
            ..Default::default()
        };

        if result.bract_running_status == RunningStatus::Running {
            return guard.finish(Ok(result));
        }

        if !self.credentials.is_root() {
            return guard.finish(Ok(result));
        }
        result.is_root = true;
        self.check_admin_group_membership(guard.span(), &mut result);
        self.check_system_folders(&mut result, douglas_folders)?;
        result.docker_running_status = self.check_docker_running_status(guard.span()).await;
        if result.docker_running_status != RunningStatus::Running {
            return guard.finish(Ok(result));
        }

        guard.finish(Ok(result))
    }

    fn check_system_folders(
        &mut self,
        result: &mut State,
        douglas_folders: &DouglasFolders,
    ) -> Result<(), ServerError> {
        for (path, expected_mode) in [
            (
                &douglas_folders.logs,
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &douglas_folders.transients,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &douglas_folders.applications,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &douglas_folders.application_services,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &douglas_folders.application_mounts,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &douglas_folders.configs,
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ] {
            self.check_system_folder(result, path, expected_mode)?;
        }
        Ok(())
    }

    fn check_admin_group_membership(&mut self, span: &Span, result: &mut State) {
        let (non_sudoer, valid_non_sudoer) = self.get_non_sudoer(span);
        if self.credentials.group_exists(DOUGLAS_ADMIN_GROUP) {
            if valid_non_sudoer
                && !self
                    .credentials
                    .group_memberships(DOUGLAS_ADMIN_GROUP)
                    .contains(&non_sudoer)
            {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement {
                        group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                        user_name: non_sudoer,
                    });
            }
        } else {
            result.groups_missing.push(DOUGLAS_ADMIN_GROUP.to_string());
            if valid_non_sudoer {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement {
                        group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                        user_name: non_sudoer,
                    });
            }
        }
    }

    fn check_system_folder(
        &self,
        state: &mut State,
        system_folder: &Path,
        expected_mode: Modes,
    ) -> Result<(), ServerError> {
        if self.folder.exists(system_folder) {
            let (owning_user, owning_group) = self
                .permissions
                .get_user_and_group_ownership(system_folder)?;
            if owning_user != credentials::ROOT_USER_NAME || owning_group != DOUGLAS_ADMIN_GROUP {
                state
                    .folders_missing_ownership
                    .push(FolderOwnershipRequirement {
                        path: system_folder.to_path_buf(),
                        owning_user_name: credentials::ROOT_USER_NAME.to_string(),
                        owning_group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                    });
            }

            let actual_mode = self.permissions.get_mode(system_folder)?;
            if actual_mode != expected_mode {
                state.folders_missing_mode.push(FolderModeRequirement {
                    path: system_folder.to_path_buf(),
                    mode: expected_mode,
                });
            }
        } else {
            state.folders_missing.push(system_folder.to_path_buf());
            state
                .folders_missing_ownership
                .push(FolderOwnershipRequirement {
                    path: system_folder.to_path_buf(),
                    owning_user_name: credentials::ROOT_USER_NAME.to_string(),
                    owning_group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                });
            state.folders_missing_mode.push(FolderModeRequirement {
                path: system_folder.to_path_buf(),
                mode: expected_mode,
            });
        }
        Ok(())
    }

    fn check_socket(
        &self,
        span: &Span,
        socket_file_name: &str,
        douglas_folders: &DouglasFolders,
    ) -> RunningStatus {
        let mut socket_path = douglas_folders.transients.clone();
        socket_path.push(socket_file_name);
        let socket_path = socket_path.as_path();
        if self.folder.exists(socket_path) {
            match UnixStream::connect(socket_path) {
                Ok(_) => RunningStatus::Running,
                Err(err) => match err.kind() {
                    std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::PermissionDenied => RunningStatus::NotRunning,
                    _ => {
                        span.message(
                            Level::Warn,
                            &format!(
                                "Could not determine status of socket '{}': '{err}'",
                                socket_path.to_str().unwrap_or_default(),
                            ),
                        );
                        RunningStatus::Unknown
                    }
                },
            }
        } else {
            RunningStatus::NotRunning
        }
    }

    fn get_non_sudoer(&self, span: &Span) -> (String, bool) {
        match self.environment_variable_reader.read("SUDO_USER") {
            Ok(user_name) => {
                let valid = user_name != credentials::ROOT_USER_NAME;
                (user_name, valid)
            }
            Err(VarError::NotPresent) => (credentials::ROOT_USER_NAME.to_string(), false),
            Err(VarError::NotUnicode(_)) => {
                span.message(Level::Warn, &format!(
                            "Could not determine initiating user?  You will need to manually add the \
                                account you wish to interact with the Douglas CLI to the '{DOUGLAS_ADMIN_GROUP}' \
                                manually!"
                        ));
                (credentials::ROOT_USER_NAME.to_string(), false)
            }
        }
    }

    async fn check_docker_running_status(&mut self, span: &Span) -> RunningStatus {
        match self.docker_ping.execute(span).await {
            Ok(()) => RunningStatus::Running,
            Err(err) => {
                span.message(Level::Warn, &format!("Docker ping failed: '{err}'"));
                RunningStatus::Unknown
            }
        }
    }
}

type Step = Box<dyn for<'a> Command<Context<'a>, Error = ServerError>>;

fn push_step(
    steps: &mut Vec<Step>,
    command: impl for<'a> Command<Context<'a>, Error = ServerError> + 'static,
) {
    steps.push(Box::new(command));
}

fn create_plan(state: State) -> Result<Vec<Step>, ServerError> {
    let mut result = Vec::new();

    if state.bract_running_status == RunningStatus::Running {
        return Ok(result);
    }
    if state.docker_running_status != RunningStatus::Running {
        return Err(ServerError::MustHaveRunningDocker);
    }

    if !state.is_root {
        return Err(ServerError::MustBeRoot);
    }

    for group_name in &state.groups_missing {
        push_step(&mut result, CreateGroup::new(group_name));
    }

    for expected in &state.group_members_missing {
        push_step(
            &mut result,
            AddUserToGroup::new(&expected.user_name, &expected.group_name),
        );
    }

    for folder in &state.folders_missing {
        push_step(&mut result, CreateFolder::new(folder.clone()));
    }

    for expeced in &state.folders_missing_ownership {
        push_step(
            &mut result,
            SetOwnership::new(
                expeced.path.clone(),
                &expeced.owning_user_name,
                &expeced.owning_group_name,
            ),
        );
    }

    for expected in &state.folders_missing_mode {
        push_step(
            &mut result,
            SetMode::new(expected.path.clone(), expected.mode),
        );
    }

    Ok(result)
}

struct CreateGroup {
    group_name: String,
}

impl CreateGroup {
    pub fn new(group_name: &str) -> Self {
        Self {
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for CreateGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("Create group '{}'", self.group_name))
    }
}

impl<'a> Command<Context<'a>> for CreateGroup {
    type Error = ServerError;

    fn name(&self) -> String {
        "Create group".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context<'a>) -> Result<(), Self::Error> {
        let guard = span
            .create_child(
                &format!("Creating group '{}'…", self.group_name),
                ScopeKind::Step,
            )
            .start_guard();

        context.credentials.create_group(&self.group_name)?;
        guard.finish(Ok(()))
    }
}

struct AddUserToGroup {
    user_name: String,
    group_name: String,
}

impl AddUserToGroup {
    pub fn new(user_name: &str, group_name: &str) -> Self {
        Self {
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for AddUserToGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "Add user '{}' to group '{}'",
            self.user_name, self.group_name
        ))
    }
}

impl<'a> Command<Context<'a>> for AddUserToGroup {
    type Error = ServerError;

    fn name(&self) -> String {
        "Add user to group".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context<'a>) -> Result<(), Self::Error> {
        let guard = span
            .create_child(
                &format!(
                    "Adding user '{}' to group '{}'…",
                    self.user_name, self.group_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .credentials
            .join_group(&self.user_name, &self.group_name)?;
        guard.finish(Ok(()))
    }
}

struct CreateFolder {
    folder: PathBuf,
}

impl CreateFolder {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }
}

impl std::fmt::Display for CreateFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("Create folder '{}'", path_to_string(&self.folder)))
    }
}

impl<'a> Command<Context<'a>> for CreateFolder {
    type Error = ServerError;

    fn name(&self) -> String {
        "Create folder".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context<'a>) -> Result<(), Self::Error> {
        let guard = span
            .create_child(
                &format!("Creating folder '{}'…", path_to_string(&self.folder)),
                ScopeKind::Step,
            )
            .start_guard();

        context.folder.create_recursively(&self.folder)?;
        guard.finish(Ok(()))
    }
}

struct SetOwnership {
    path: PathBuf,
    user_name: String,
    group_name: String,
}

impl SetOwnership {
    pub fn new(path: PathBuf, user_name: &str, group_name: &str) -> Self {
        Self {
            path,
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for SetOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "Set ownership on '{}', to group '{}' and user '{}'",
            path_to_string(&self.path),
            self.group_name,
            self.user_name
        ))
    }
}

impl<'a> Command<Context<'a>> for SetOwnership {
    type Error = ServerError;

    fn name(&self) -> String {
        "Set ownership".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context<'a>) -> Result<(), Self::Error> {
        let guard = span
            .create_child(
                &format!(
                    "Setting ownership for '{}' to group '{}' and user '{}'…",
                    path_to_string(&self.path),
                    self.group_name,
                    self.user_name,
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context.permissions.change_user_and_group_ownership(
            &self.path,
            &self.user_name,
            &self.group_name,
        )?;

        guard.finish(Ok(()))
    }
}

struct SetMode {
    path: PathBuf,
    mode: Modes,
}

impl SetMode {
    pub fn new(path: PathBuf, mode: Modes) -> Self {
        Self { path, mode }
    }
}

impl std::fmt::Display for SetMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "Set mode on '{}' to '{}'",
            path_to_string(&self.path),
            self.mode,
        ))
    }
}

impl<'a> Command<Context<'a>> for SetMode {
    type Error = ServerError;

    fn name(&self) -> String {
        "Set mode".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context<'a>) -> Result<(), Self::Error> {
        let guard = span
            .create_child(
                &format!(
                    "Setting mode on '{}' to '{}'…",
                    path_to_string(&self.path),
                    self.mode,
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context.permissions.change_mode(&self.path, &self.mode)?;

        guard.finish(Ok(()))
    }
}
