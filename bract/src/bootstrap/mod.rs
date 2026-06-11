mod pipe_reporter;

use crate::{BootstrapError, bootstrap::pipe_reporter::PipeReporter};
use blueprint::{Command, CommandExecutor, JournalingExecutor, RunningStatus};
use config::{DouglasFolders, constants::DOUGLAS_ADMIN_GROUP};
use credentials::Credentials;
use file_system::{Folder, Modes, Permissions, path_to_string};
use log::{BufferedFileReporter, Level, Reporter, ScopeKind, Span, TeeReporter};
use os::EnvironmentVariableReader;
use std::{
    env::VarError,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
};

pub async fn bootstrap(
    reporting_fd: i32,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    environment_variable_reader: &dyn EnvironmentVariableReader,
    douglas_folders: &DouglasFolders,
) -> Result<(), BootstrapError> {
    let boot_reporter: Arc<dyn Reporter> = {
        let mut sinks: Vec<Box<dyn Reporter>> = vec![Box::new(BufferedFileReporter::new(
            douglas_folders.log_file("bract"),
        ))];

        sinks.push(Box::new(unsafe { PipeReporter::from_raw_fd(reporting_fd) }));
        Arc::new(TeeReporter::new(sinks))
    };

    let guard = Span::new(
        Arc::clone(&boot_reporter),
        "Bootstrapping douglas-bract system",
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
                return guard.finish(Err(BootstrapError::MustHaveRunningDocker));
            }
        };

    let state = {
        let mut state_observer = StateObserver::new(
            credentials,
            folder,
            permissions,
            environment_variable_reader,
            &mut *docker_ping,
        );
        state_observer
            .discover(guard.span(), douglas_folders)
            .await?
    };

    if !state.is_root {
        guard.span().message(Level::Warn, "Must be root");
        return guard.finish(Err(BootstrapError::MustBeRoot));
    }

    if state.docker_running_status == RunningStatus::NotRunning {
        guard.span().message(Level::Warn, "Docker must be running");
        return guard.finish(Err(BootstrapError::MustHaveRunningDocker));
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
            guard.finish(Err(BootstrapError::FailedBoostrap(Vec::new())))
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
    ) -> Result<State, BootstrapError> {
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
    ) -> Result<(), BootstrapError> {
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
    ) -> Result<(), BootstrapError> {
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

type Step = Box<dyn for<'a> Command<Context<'a>, Error = BootstrapError>>;

fn push_step(
    steps: &mut Vec<Step>,
    command: impl for<'a> Command<Context<'a>, Error = BootstrapError> + 'static,
) {
    steps.push(Box::new(command));
}

fn create_plan(state: State) -> Result<Vec<Step>, BootstrapError> {
    let mut result = Vec::new();

    if state.bract_running_status == RunningStatus::Running {
        return Ok(result);
    }
    if state.docker_running_status != RunningStatus::Running {
        return Err(BootstrapError::MustHaveRunningDocker);
    }

    if !state.is_root {
        return Err(BootstrapError::MustBeRoot);
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
    type Error = BootstrapError;

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
    type Error = BootstrapError;

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
    type Error = BootstrapError;

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
    type Error = BootstrapError;

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
    type Error = BootstrapError;

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
