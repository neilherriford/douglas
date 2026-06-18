use blueprint::commands::{AddUserToGroup, CreateFolder, CreateGroup, SetMode, SetOwnership};
use blueprint::{
    Command, CommandExecutor, FolderModeRequirement, FolderOwnershipRequirement,
    GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, JournalingExecutor,
    RunningStatus,
};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
use file_system::{Folder, Modes, Permissions};
use log::{BufferedFileReporter, Level, PipeReporter, Reporter, ScopeKind, Span, TeeReporter};
use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::BootstrapError;

pub async fn bootstrap(
    reporting_fd: i32,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
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
        let mut state_observer =
            StateObserver::new(credentials, folder, permissions, &mut *docker_ping);
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

struct Context<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
    permissions: &'a dyn Permissions,
}

impl<'a> HasCredentials for Context<'a> {
    fn credentials(&self) -> &dyn Credentials {
        self.credentials
    }
}

impl<'a> HasFolder for Context<'a> {
    fn folder(&self) -> &dyn Folder {
        self.folder
    }
}

impl<'a> HasPermissions for Context<'a> {
    fn permissions(&self) -> &dyn Permissions {
        self.permissions
    }
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
    docker_ping: &'a mut dyn docker::Ping,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        folder: &'a dyn Folder,
        permissions: &'a dyn Permissions,
        docker_ping: &'a mut dyn docker::Ping,
    ) -> Self {
        Self {
            credentials,
            folder,
            permissions,
            docker_ping,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, BootstrapError> {
        let guard = span
            .create_child(
                "Starting bract system, discovering current state",
                ScopeKind::Phase,
            )
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

type Step = Box<dyn for<'a> Command<Context<'a>>>;

fn push_step(steps: &mut Vec<Step>, command: impl for<'a> Command<Context<'a>> + 'static) {
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
