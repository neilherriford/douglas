use blueprint::{Command, CommandExecutor, JournalingExecutor};
use config::{
    DouglasFolders,
    constants::{DOUGLAS_ADMIN_GROUP, DOUGLAS_APP_GROUP},
};
use credentials::{Credentials, CredentialsError};
use file_system::{FileReader, FileSystemError, Folder, Modes, Permissions, path_to_string};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{EnvironmentVariableReader, Os, OsError};
use std::{
    env::VarError,
    num::ParseIntError,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DouglasBootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Invalid PID: {0}")]
    InvalidPid(#[from] ParseIntError),
    #[error("Could not determine pid liveliness {0}")]
    PidLiveliness(#[from] OsError),
    #[error("Could not determine executing path")]
    CouldNotDetermineExecutingPath,
    #[error("Invalid path")]
    InvalidPath(PathBuf),
}

type Step = Box<dyn Command<Context, Error = DouglasBootstrapError>>;

fn push_step(
    steps: &mut Vec<Step>,
    command: impl Command<Context, Error = DouglasBootstrapError> + 'static,
) {
    steps.push(Box::new(command));
}

struct Context {
    credentials: Arc<dyn Credentials>,
    folder: Arc<dyn Folder>,
    permissions: Arc<dyn Permissions>,
    os: Arc<dyn Os>,
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

#[derive(Default)]
struct State {
    is_root: bool,
    groups_missing: Vec<String>,
    group_members_missing: Vec<GroupMembershipRequirement>,
    folders_missing: Vec<PathBuf>,
    folders_missing_ownership: Vec<FolderOwnershipRequirement>,
    folders_missing_mode: Vec<FolderModeRequirement>,
    bract_pid: Option<u32>,
}

struct StateObserver {
    credentials: Arc<dyn Credentials>,
    file_reader: Arc<dyn FileReader>,
    permissions: Arc<dyn Permissions>,
    os: Arc<dyn Os>,
    environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
}

impl StateObserver {
    pub fn new(
        credentials: Arc<dyn Credentials>,
        file_reader: Arc<dyn FileReader>,
        permissions: Arc<dyn Permissions>,
        os: Arc<dyn Os>,
        environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    ) -> Self {
        Self {
            credentials,
            file_reader,
            permissions,
            os,
            environment_variable_reader,
        }
    }

    pub fn discover(
        &self,
        span: &Span,
        config: &DouglasFolders,
    ) -> Result<State, DouglasBootstrapError> {
        let guard = span
            .create_child("CLI Start: Discover", ScopeKind::Phase)
            .start_guard();
        let mut result = State::default();

        if !self.credentials.is_root() {
            return guard.finish(Ok(result));
        }
        result.is_root = true;

        let non_sudoer_user = match self.get_sudoer(guard.span()) {
            Some(sudoer) => sudoer,
            None => credentials::ROOT_USER_NAME.to_string(),
        };
        let valid_non_sudoer_user = non_sudoer_user != credentials::ROOT_USER_NAME;

        if self.credentials.group_exists(DOUGLAS_ADMIN_GROUP) {
            if valid_non_sudoer_user
                && !self
                    .credentials
                    .group_memberships(DOUGLAS_ADMIN_GROUP)
                    .contains(&non_sudoer_user)
            {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement {
                        group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                        user_name: non_sudoer_user,
                    });
            }
        } else {
            result.groups_missing.push(DOUGLAS_ADMIN_GROUP.to_string());
            if valid_non_sudoer_user {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement {
                        group_name: DOUGLAS_ADMIN_GROUP.to_string(),
                        user_name: non_sudoer_user,
                    });
            }
        }

        for (path, expected_mode) in [
            (
                &config.logs,
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &config.transients,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &config.applications,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &config.application_services,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &config.application_mounts,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                &config.configs,
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ] {
            self.check_system_folder(&mut result, path, expected_mode)?;
        }

        let pid = self.get_bract_pid(&config.transients)?;

        result.bract_pid = if let Some(pid) = pid
            && self.os.is_active_pid(pid)?
        {
            Some(pid)
        } else {
            None
        };

        guard.finish(Ok(result))
    }

    fn check_system_folder(
        &self,
        state: &mut State,
        system_folder: &Path,
        expected_mode: Modes,
    ) -> Result<(), DouglasBootstrapError> {
        if self.file_reader.exists(system_folder) {
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

    fn get_sudoer(&self, span: &Span) -> Option<String> {
        match self.environment_variable_reader.read("SUDO_USER") {
            Ok(user_name) => Some(user_name),
            Err(VarError::NotPresent) => None,
            Err(VarError::NotUnicode(_)) => {
                span.message(Level::Warn, &format!(
                            "Could not determine initiating user?  You will need to manually add the \
                                account you wish to interact with the Douglas CLI to the '{DOUGLAS_ADMIN_GROUP}' \
                                manually!"
                        ));
                None
            }
        }
    }

    fn get_bract_pid(&self, sockets_root: &PathBuf) -> Result<Option<u32>, DouglasBootstrapError> {
        let mut expected_path = sockets_root.clone();
        expected_path.push("bract.pid");

        if !self.file_reader.exists(&expected_path) {
            return Ok(None);
        }

        let raw_pid = self.file_reader.read_all(&expected_path)?;
        let pid: u32 = raw_pid.parse()?;

        Ok(Some(pid))
    }
}

fn create_plan(state: State) -> Result<Vec<Step>, DouglasBootstrapError> {
    let mut result = Vec::new();

    if !state.is_root {
        return Err(DouglasBootstrapError::MustBeRoot);
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

    if let Some(pid) = state.bract_pid
        && (!state.groups_missing.is_empty()
            || !state.group_members_missing.is_empty()
            || !state.folders_missing.is_empty()
            || !state.folders_missing_ownership.is_empty()
            || !state.folders_missing_mode.is_empty())
    {
        push_step(&mut result, KillPid::new(pid));
    }

    Ok(result)
}

// // // // // // // // // // // // // // // // // // // // // // //

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

impl Command<Context> for CreateGroup {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Create group".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
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

// // // // // // // // // // // // // // // // // // // // // // //

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

impl Command<Context> for AddUserToGroup {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Add user to group".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
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

// // // // // // // // // // // // // // // // // // // // // // //

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

impl Command<Context> for CreateFolder {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Add user to group".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
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

// // // // // // // // // // // // // // // // // // // // // // //

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

impl Command<Context> for SetOwnership {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Set ownership".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
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

// // // // // // // // // // // // // // // // // // // // // // //

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

impl Command<Context> for SetMode {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Set ownership".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
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

// // // // // // // // // // // // // // // // // // // // // // //

struct KillPid {
    pid: u32,
}

impl KillPid {
    pub fn new(pid: u32) -> Self {
        Self { pid }
    }
}

impl std::fmt::Display for KillPid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("Kill pid  '{}'", self.pid,))
    }
}

impl Command<Context> for KillPid {
    type Error = DouglasBootstrapError;

    fn name(&self) -> String {
        "Kill pid".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
        let guard = span
            .create_child(&format!("Killing pid '{}'…", self.pid,), ScopeKind::Step)
            .start_guard();
        context.os.kill(self.pid)?;
        guard.finish(Ok(()))
    }
}

// // // // // // // // // // // // // // // // // // // // // // //

pub async fn cli_start(
    reporter: Arc<dyn Reporter>,
    plan_only: bool,
    credentials: Arc<dyn Credentials>,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    permissions: Arc<dyn Permissions>,
    os: Arc<dyn Os>,
    environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    config: DouglasFolders,
) {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();
    let state_observer = StateObserver::new(
        Arc::clone(&credentials),
        file_reader,
        Arc::clone(&permissions),
        Arc::clone(&os),
        environment_variable_reader,
    );
    let state = match state_observer.discover(guard.span(), &config) {
        Ok(s) => s,
        Err(e) => {
            guard.span().message(Level::Warn, &e.to_string());
            return;
        }
    };

    let plan = match create_plan(state) {
        Ok(p) => p,
        Err(e) => {
            guard.span().message(Level::Warn, &e.to_string());
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
        credentials,
        folder,
        permissions,
        os,
    };

    match executor.run(
        &guard
            .span()
            .create_child("Executing plan", ScopeKind::Phase),
        &mut context,
        plan,
    ) {
        blueprint::ExecutionResult::Success => guard.finish_with_outcome(Outcome::Ok),
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
            guard.finish_with_outcome(Outcome::Failed);
        }
    }
}
