use crate::commands::cli_start::CliBootstrapError::SpawnError;
use blueprint::{
    Command, CommandExecutor, ExecutionResult, FolderModeRequirement, FolderOwnershipRequirement,
    GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, JournalingExecutor,
    RunningStatus,
    commands::{AddUserToGroup, CreateFolder, CreateGroup, CreateUser, SetMode, SetOwnership},
};
use command_fds::{CommandFdExt, FdMapping, FdMappingCollision};
use config::DouglasFolders;
use credentials::{
    Credentials, CredentialsError,
    well_known::{DOUGLAS_ADMIN_GROUP, DOUGLAS_RESIN_GROUP, DOUGLAS_RESIN_USER},
};
use file_system::{FileReader, FileSystemError, Folder, Modes, Permissions};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{EnvironmentVariableReader, Os, OsError};
use os_pipe::{PipeReader, PipeWriter};
use std::{
    env::VarError,
    num::ParseIntError,
    os::{fd::OwnedFd, unix::io::AsRawFd},
    path::Path,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliBootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Pipe required")]
    PipeRequired,
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Invalid PID: {0}")]
    InvalidPid(#[from] ParseIntError),
    #[error("Could not determine pid liveliness {0}")]
    PidLiveliness(#[from] OsError),
    #[error("Pipe error: {0}")]
    PipeError(#[from] std::io::Error),
    #[error("Spawn error: {0}")]
    SpawnError(#[from] FdMappingCollision),
    #[error("Timed out waiting for bract to start (5 minutes exceeded)")]
    BractStartTimeout,
}

type Step = Box<dyn for<'a> Command<Context<'a>>>;

fn push_step(steps: &mut Vec<Step>, command: impl for<'a> Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct Context<'a> {
    os: &'a dyn Os,
    credentials: &'a dyn Credentials,
    permissions: &'a dyn Permissions,
    folder: &'a dyn Folder,
    pipe_reader: Option<PipeReader>,
    pipe_writer: Option<PipeWriter>,
}

impl HasCredentials for Context<'_> {
    fn credentials(&self) -> &dyn Credentials {
        self.credentials
    }
}

impl HasFolder for Context<'_> {
    fn folder(&self) -> &dyn Folder {
        self.folder
    }
}

impl HasPermissions for Context<'_> {
    fn permissions(&self) -> &dyn Permissions {
        self.permissions
    }
}

#[derive(Default)]
struct State {
    is_root: bool,
    bract_running_status: RunningStatus,
    resin_running_status: RunningStatus,
    users_missing: Vec<String>,
    groups_missing: Vec<String>,
    group_members_missing: Vec<GroupMembershipRequirement>,
    folders_missing: Vec<PathBuf>,
    folders_missing_ownership: Vec<FolderOwnershipRequirement>,
    folders_missing_mode: Vec<FolderModeRequirement>,
    //TODO: core apps installed
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    permissions: &'a dyn Permissions,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
    file_reader: &'a dyn FileReader,
    os: &'a dyn Os,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        permissions: &'a dyn Permissions,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
        file_reader: &'a dyn FileReader,
        os: &'a dyn Os,
    ) -> Self {
        Self {
            credentials,
            permissions,
            environment_variable_reader,
            file_reader,
            os,
        }
    }

    pub fn discover(
        &mut self,
        span: &Span,
        config: &DouglasFolders,
    ) -> Result<State, CliBootstrapError> {
        let guard = span
            .create_child(
                "Starting douglas system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        if !self.credentials.is_root() {
            return Ok(State::default());
        }

        let mut result = State {
            is_root: true,
            bract_running_status: self.get_pid_running_status(&config.transients, "bract.pid")?,
            resin_running_status: self.get_pid_running_status(&config.transients, "resin.pid")?,
            ..Default::default()
        };

        self.check_admin_group_membership(guard.span(), &mut result);
        self.check_resin_credentials(&mut result);
        self.check_resin_root(config, &mut result)?;

        guard.finish(Ok(result))
    }

    fn check_resin_root(
        &mut self,
        config: &DouglasFolders,
        result: &mut State,
    ) -> Result<(), CliBootstrapError> {
        if self.file_reader.exists(&config.resin) {
            if self
                .permissions
                .get_user_and_group_ownership(&config.resin)?
                != (
                    DOUGLAS_RESIN_USER.to_string(),
                    DOUGLAS_RESIN_GROUP.to_string(),
                )
            {
                result
                    .folders_missing_ownership
                    .push(FolderOwnershipRequirement::new(
                        config.resin.clone(),
                        DOUGLAS_RESIN_USER,
                        DOUGLAS_RESIN_GROUP,
                    ));
            }
            if self.permissions.get_mode(&config.resin)? != Modes::OwnerReadWrite {
                result.folders_missing_mode.push(FolderModeRequirement {
                    path: config.resin.clone(),
                    mode: Modes::OwnerReadWrite,
                });
            }
        } else {
            result.folders_missing.push(config.resin.clone());
            result
                .folders_missing_ownership
                .push(FolderOwnershipRequirement::new(
                    config.resin.clone(),
                    DOUGLAS_RESIN_USER,
                    DOUGLAS_RESIN_GROUP,
                ));
            result.folders_missing_mode.push(FolderModeRequirement {
                path: config.resin.clone(),
                mode: Modes::OwnerReadWrite,
            });
        }

        Ok(())
    }

    fn check_resin_credentials(&mut self, result: &mut State) {
        if !self.credentials.user_exists(DOUGLAS_RESIN_USER) {
            result.users_missing.push(DOUGLAS_RESIN_USER.to_string());
        }

        if self.credentials.group_exists(DOUGLAS_RESIN_GROUP) {
            if !self
                .credentials
                .group_memberships(DOUGLAS_RESIN_GROUP)
                .contains(&DOUGLAS_RESIN_USER.to_string())
            {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement::new(
                        DOUGLAS_RESIN_GROUP,
                        DOUGLAS_RESIN_USER,
                    ));
            }
        } else {
            result.groups_missing.push(DOUGLAS_RESIN_GROUP.to_string());
        }
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
                    .push(GroupMembershipRequirement::new(
                        DOUGLAS_ADMIN_GROUP,
                        &non_sudoer,
                    ));
            }
        } else {
            result.groups_missing.push(DOUGLAS_ADMIN_GROUP.to_string());
            if valid_non_sudoer {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement::new(
                        DOUGLAS_ADMIN_GROUP,
                        &non_sudoer,
                    ));
            }
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

    fn get_pid_running_status(
        &self,
        sockets_root: &Path,
        name: &str,
    ) -> Result<RunningStatus, CliBootstrapError> {
        let mut expected_path = sockets_root.to_path_buf();
        expected_path.push(name);

        if !self.file_reader.exists(&expected_path) {
            return Ok(RunningStatus::NotRunning);
        }

        let raw_pid = self.file_reader.read_all(&expected_path)?;
        let pid: u32 = raw_pid.trim().parse()?;

        if self.os.is_active_pid(pid)? {
            Ok(RunningStatus::Running)
        } else {
            Ok(RunningStatus::NotRunning)
        }
    }
}

fn create_plan(state: &State) -> Result<Vec<Step>, CliBootstrapError> {
    if !state.is_root {
        return Err(CliBootstrapError::MustBeRoot);
    }

    let mut result = Vec::new();

    for missing_group in &state.groups_missing {
        push_step(&mut result, CreateGroup::new(missing_group));
    }

    for missing_user in &state.users_missing {
        push_step(&mut result, CreateUser::new(missing_user, missing_user));
    }

    for membership in &state.group_members_missing {
        push_step(
            &mut result,
            AddUserToGroup::new(&membership.user_name, &membership.group_name),
        );
    }

    for folder in &state.folders_missing {
        push_step(&mut result, CreateFolder::new(folder.clone()));
    }

    for folder_owner in &state.folders_missing_ownership {
        push_step(
            &mut result,
            SetOwnership::new(
                folder_owner.path.clone(),
                &folder_owner.owning_user_name,
                &folder_owner.owning_group_name,
            ),
        );
    }

    for folder_mode in &state.folders_missing_mode {
        push_step(
            &mut result,
            SetMode::new(folder_mode.path.clone(), folder_mode.mode),
        );
    }

    if state.resin_running_status != RunningStatus::Running {
        push_step(&mut result, StartResin::new());
    }

    if state.bract_running_status != RunningStatus::Running {
        push_step(&mut result, CreatePipe::new());
        push_step(&mut result, StartBract::new());
    }

    Ok(result)
}

struct CreatePipe {}

impl CreatePipe {
    pub fn new() -> Self {
        Self {}
    }
}

impl std::fmt::Display for CreatePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Create pipe")
    }
}

impl<'a> Command<Context<'a>> for CreatePipe {
    fn name(&self) -> String {
        "Create Pipe".to_string()
    }

    fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child("Creating pipe…", ScopeKind::Step)
            .start_guard();

        let (read_fd, write_fd) = os_pipe::pipe()?;
        context.pipe_reader = Some(read_fd);
        context.pipe_writer = Some(write_fd);

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct StartResin {}

impl StartResin {
    pub fn new() -> Self {
        Self {}
    }
}

impl std::fmt::Display for StartResin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Start resin")
    }
}

impl<'a> Command<Context<'a>> for StartResin {
    fn name(&self) -> String {
        "Start resin".to_string()
    }

    fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child("Starting resin…", ScopeKind::Step)
            .start_guard();

        std::process::Command::new(context.os.current_executable()?)
            .args(["service", "resin"])
            .spawn()?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct StartBract {}

impl StartBract {
    pub fn new() -> Self {
        Self {}
    }
}

impl std::fmt::Display for StartBract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Start bract")
    }
}

impl<'a> Command<Context<'a>> for StartBract {
    fn name(&self) -> String {
        "Start bract".to_string()
    }

    fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child("Starting bract…", ScopeKind::Step)
            .start_guard();

        let Some(pipe_writer) = context.pipe_writer.take() else {
            return Err(Box::new(CliBootstrapError::PipeRequired));
        };
        let Some(pipe_reader) = context.pipe_reader.take() else {
            return Err(Box::new(CliBootstrapError::PipeRequired));
        };

        let fd = pipe_writer.as_raw_fd();
        match std::process::Command::new(context.os.current_executable()?)
            .args(["service", "bract", "--notify-fd", &fd.to_string()])
            .fd_mappings(vec![FdMapping {
                parent_fd: OwnedFd::from(pipe_writer),
                child_fd: fd,
            }]) {
            Ok(cmd) => {
                cmd.spawn()?;
            }
            Err(err) => return Err(Box::new(SpawnError(err))),
        }

        /*
         * Read bract's event stream on a background thread,
         * forward events to the reporter so they appear in the ui,
         * bail with a five minute timeout.
         */
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(pipe_reader);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
            // Sender drops here — receiver will see Disconnected when pipe closes.
        });

        let deadline = Instant::now() + Duration::from_secs(5 * 60);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Box::new(CliBootstrapError::BractStartTimeout));
            }
            match rx.recv_timeout(remaining) {
                Ok(line) => {
                    let Ok(event) = serde_json::from_str::<log::Event>(&line) else {
                        continue;
                    };
                    span.reporter.emit(event);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // pipe closed — bract ready
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(Box::new(CliBootstrapError::BractStartTimeout));
                }
            }
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub async fn cli_start(
    reporter: Arc<dyn Reporter>,
    plan_only: bool,
    credentials: Arc<dyn Credentials>,
    permissions: Arc<dyn Permissions>,
    environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    os: Arc<dyn Os>,
    douglas_folders: DouglasFolders,
) {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();
    let mut state_observer = StateObserver::new(
        credentials.as_ref(),
        permissions.as_ref(),
        environment_variable_reader.as_ref(),
        file_reader.as_ref(),
        os.as_ref(),
    );
    let state = match state_observer.discover(guard.span(), &douglas_folders) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    let plan = match create_plan(&state) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    guard
        .span()
        .plan_hint(plan.iter().map(std::string::ToString::to_string).collect());

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return;
    }

    let mut executor = JournalingExecutor::new();
    let mut context = Context {
        os: os.as_ref(),
        credentials: credentials.as_ref(),
        folder: folder.as_ref(),
        permissions: permissions.as_ref(),
        pipe_reader: None,
        pipe_writer: None,
    };

    match executor.run(
        &guard
            .span()
            .create_child("Executing plan", ScopeKind::Phase),
        &mut context,
        plan,
    ) {
        ExecutionResult::Success => {
            guard.finish_with_outcome(Outcome::Ok);
        }
        ExecutionResult::Failed {
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
            guard.finish_with_outcome(Outcome::Failed);
        }
    }
}
