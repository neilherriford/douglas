use crate::util::{require, spawn_service, wait_until_running};
use async_trait::async_trait;
use blueprint::{
    Command, GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, RunningStatus,
    bootstrap::{execute_plan, resolve_plan},
    commands::{AddUserToGroup, CreateFolder, CreateGroup},
    listener::{LivenessCheck, check_liveness},
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, discover_service_state,
        plan_service_bootstrap,
    },
};
use command_fds::FdMappingCollision;
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
use file_system::{FileSystemError, Folder, Modes, Permissions};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{EnvironmentVariableReader, Os};
use os_pipe::{PipeReader, PipeWriter};
use std::{collections::HashMap, env::VarError, path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Pipe required")]
    PipeRequired,
    #[error("Spawn error: {0}")]
    SpawnError(#[from] FdMappingCollision),
    #[error("Timed out waiting for {0} to start (5 minutes exceeded)")]
    StartTimeout(String),
    #[error("Service '{0}' has no configured liveness check")]
    MissingLivenessCheck(String),
    #[error("Unknown service '{0}'")]
    UnknownService(String),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct DouglasService {
    name: &'static str,
    bootstrap_reporting: BootstrapReporting,
    liveness: LivenessCheck,
    definition: ServiceDefinition,
}

pub(crate) fn liveness_check(
    service_name: &str,
    douglas_folders: &DouglasFolders,
) -> Result<LivenessCheck, BootstrapError> {
    let definition = if service_name == config::services::BRACT {
        bract::service_definition(douglas_folders)
    } else if service_name == config::services::SEEDBANK {
        seedbank::service_definition(douglas_folders)
    } else if service_name == config::services::RESIN {
        resin::service_definition(douglas_folders)
    } else {
        return Err(BootstrapError::UnknownService(service_name.to_string()));
    };

    require_liveness(&definition, service_name)
}

fn require_liveness(
    definition: &ServiceDefinition,
    service_name: &str,
) -> Result<LivenessCheck, BootstrapError> {
    definition
        .liveness
        .clone()
        .ok_or_else(|| BootstrapError::MissingLivenessCheck(service_name.to_string()))
}

fn known_services(douglas_folders: &DouglasFolders) -> Result<Vec<DouglasService>, BootstrapError> {
    let bract_definition = bract::service_definition(douglas_folders);
    let bract_liveness = require_liveness(&bract_definition, config::services::BRACT)?;

    let seedbank_definition = seedbank::service_definition(douglas_folders);
    let seedbank_liveness = require_liveness(&seedbank_definition, config::services::SEEDBANK)?;

    let resin_definition = resin::service_definition(douglas_folders);
    let resin_liveness = require_liveness(&resin_definition, config::services::RESIN)?;

    let woodward_definition = woodward::service_definition(douglas_folders);
    let woodward_liveness = require_liveness(&woodward_definition, config::services::WOODWARD)?;

    Ok(vec![
        DouglasService {
            name: config::services::BRACT,
            bootstrap_reporting: bract_definition.bootstrap_reporting,
            liveness: bract_liveness,
            definition: bract_definition,
        },
        DouglasService {
            name: config::services::RESIN,
            bootstrap_reporting: resin_definition.bootstrap_reporting,
            liveness: resin_liveness,
            definition: resin_definition,
        },
        DouglasService {
            name: config::services::SEEDBANK,
            bootstrap_reporting: seedbank_definition.bootstrap_reporting,
            liveness: seedbank_liveness,
            definition: seedbank_definition,
        },
        DouglasService {
            name: config::services::WOODWARD,
            bootstrap_reporting: woodward_definition.bootstrap_reporting,
            liveness: woodward_liveness,
            definition: woodward_definition,
        },
    ])
}

struct Context<'a> {
    os: &'a dyn Os,
    credentials: &'a dyn Credentials,
    permissions: &'a dyn Permissions,
    folder: &'a dyn Folder,
    pipes: HashMap<String, (PipeReader, PipeWriter)>,
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
struct Work {
    groups_missing: Vec<String>,
    group_members_missing: Vec<GroupMembershipRequirement>,
    services_needing_start: Vec<(DouglasService, ServiceState)>,
    cli_log_dir_missing: Option<PathBuf>,
}

enum State {
    NotRoot,
    Root(Work),
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
    folder: &'a dyn Folder,
    permissions: &'a dyn Permissions,
}

impl StateObserver<'_> {
    pub fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, BootstrapError> {
        let guard = span
            .create_child(
                "Starting douglas system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        if !self.credentials.is_root() {
            return guard.finish(Ok(State::NotRoot));
        }

        let mut result = Work::default();

        self.check_admin_group_membership(guard.span(), &mut result);

        let cli_log_dir = douglas_folders.log_dir(config::DOUGLAS_CLI_LOG_NAME);
        if !self.folder.exists(&cli_log_dir) {
            result.cli_log_dir_missing = Some(cli_log_dir);
        }

        let services = match known_services(douglas_folders) {
            Ok(services) => services,
            Err(err) => return guard.finish(Err(err)),
        };

        for service in services {
            let status = check_liveness(guard.span(), &service.liveness);
            if status != RunningStatus::Running {
                let service_state = match discover_service_state(
                    &service.definition,
                    self.credentials,
                    self.folder,
                    self.permissions,
                ) {
                    Ok(service_state) => service_state,
                    Err(err) => return guard.finish(Err(err.into())),
                };
                result.services_needing_start.push((service, service_state));
            }
        }

        guard.finish(Ok(State::Root(result)))
    }

    fn check_admin_group_membership(&mut self, span: &Span, result: &mut Work) {
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
}

fn create_plan<'a>(state: State) -> Result<Vec<Step<'a>>, BootstrapError> {
    let State::Root(state) = state else {
        return Err(BootstrapError::MustBeRoot);
    };

    let mut result = Vec::new();

    for group_name in &state.groups_missing {
        push_step(&mut result, CreateGroup::new(group_name));
    }

    for membership in &state.group_members_missing {
        push_step(
            &mut result,
            AddUserToGroup::new(&membership.user_name, &membership.group_name),
        );
    }

    if let Some(cli_log_dir) = state.cli_log_dir_missing {
        push_step(&mut result, CreateFolder::new(cli_log_dir));
    }

    for (service, service_state) in &state.services_needing_start {
        for step in plan_service_bootstrap::<Context<'a>>(&service.definition, service_state) {
            result.push(step);
        }
    }

    for (service, _) in &state.services_needing_start {
        if matches!(service.bootstrap_reporting, BootstrapReporting::Pipe) {
            push_step(&mut result, CreatePipe::new(service.name));
        }
    }

    for (service, _) in &state.services_needing_start {
        let needs_reporting_pipe = matches!(service.bootstrap_reporting, BootstrapReporting::Pipe);
        push_step(
            &mut result,
            StartService::new(service.name, needs_reporting_pipe),
        );
    }

    for (service, _) in state.services_needing_start {
        push_step(
            &mut result,
            WaitForServiceReady::new(service.name, service.liveness),
        );
    }

    Ok(result)
}

struct CreatePipe {
    service_name: String,
}

impl CreatePipe {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
        }
    }
}

impl std::fmt::Display for CreatePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create pipe for {}", self.service_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreatePipe {
    fn name(&self) -> String {
        "Create Pipe".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Creating pipe…", ScopeKind::Step)
            .start_guard();

        let (reader, writer) = os_pipe::pipe()?;
        context
            .pipes
            .insert(self.service_name.clone(), (reader, writer));

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct StartService {
    name: &'static str,
    needs_reporting_pipe: bool,
}

impl StartService {
    pub fn new(name: &'static str, needs_reporting_pipe: bool) -> Self {
        Self {
            name,
            needs_reporting_pipe,
        }
    }
}

impl std::fmt::Display for StartService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Start {}", self.name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartService {
    fn name(&self) -> String {
        format!("Start {}", self.name)
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(&format!("Starting {}…", self.name), ScopeKind::Step)
            .start_guard();

        let pipe = if self.needs_reporting_pipe {
            let Some(pipe) = context.pipes.remove(self.name) else {
                return Err(Box::new(BootstrapError::PipeRequired));
            };
            Some(pipe)
        } else {
            None
        };

        spawn_service(
            self.name,
            pipe,
            self.needs_reporting_pipe,
            context.os,
            guard.span(),
        )?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct WaitForServiceReady {
    name: &'static str,
    liveness: LivenessCheck,
}

impl WaitForServiceReady {
    pub fn new(name: &'static str, liveness: LivenessCheck) -> Self {
        Self { name, liveness }
    }
}

impl std::fmt::Display for WaitForServiceReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wait for {} to be ready", self.name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for WaitForServiceReady {
    fn name(&self) -> String {
        format!("Wait for {}", self.name)
    }

    async fn run(
        &mut self,
        span: &Span,
        _context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(&format!("Waiting for {}…", self.name), ScopeKind::Step)
            .start_guard();

        if !wait_until_running(&self.liveness, guard.span()).await {
            return Err(Box::new(BootstrapError::StartTimeout(
                self.name.to_string(),
            )));
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub(crate) struct Dependencies {
    pub credentials: Arc<dyn Credentials>,
    pub permissions: Arc<dyn Permissions>,
    pub environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    pub folder: Arc<dyn Folder>,
    pub os: Arc<dyn Os>,
    pub douglas_folders: DouglasFolders,
}

pub async fn perform(reporter: Arc<dyn Reporter>, plan_only: bool, deps: Dependencies) -> bool {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut state_observer = StateObserver {
        credentials: deps.credentials.as_ref(),
        environment_variable_reader: deps.environment_variable_reader.as_ref(),
        folder: deps.folder.as_ref(),
        permissions: deps.permissions.as_ref(),
    };
    let Some(state) = require(
        &guard,
        "Failed to discover current state",
        state_observer.discover(guard.span(), &deps.douglas_folders),
    ) else {
        return false;
    };

    let Some(plan) = require(
        &guard,
        "Failed to resolve plan",
        resolve_plan(guard.span(), create_plan(state)),
    ) else {
        return false;
    };

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return true;
    }

    let mut context = Context {
        os: deps.os.as_ref(),
        credentials: deps.credentials.as_ref(),
        folder: deps.folder.as_ref(),
        permissions: deps.permissions.as_ref(),
        pipes: HashMap::new(),
    };

    let result = execute_plan(guard.span(), plan, &mut context, |_reason| ()).await;

    if result.is_ok()
        && let Err(err) = ensure_supervised_heartbeat_dirs_accessible(
            deps.permissions.as_ref(),
            &deps.douglas_folders,
        )
    {
        guard.span().message(Level::Warn, &err.to_string());
        guard.finish_with_outcome(Outcome::Failed);
        return false;
    }

    if result.is_ok() {
        guard.finish_with_outcome(Outcome::Ok);
        true
    } else {
        guard.finish_with_outcome(Outcome::Failed);
        false
    }
}

fn ensure_supervised_heartbeat_dirs_accessible(
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
) -> Result<(), FileSystemError> {
    for (service_name, service_user) in [
        (config::services::BRACT, credentials::ROOT_USER_NAME),
        (resin::RESIN, resin::DOUGLAS_RESIN_USER),
        (seedbank::SEEDBANK, seedbank::DOUGLAS_SEEDBANK_USER),
    ] {
        let heartbeat_dir = douglas_folders.heartbeat_dir(service_name);
        permissions.change_user_and_group_ownership(
            &heartbeat_dir,
            service_user,
            DOUGLAS_ADMIN_GROUP,
        )?;
        permissions.change_mode(
            &heartbeat_dir,
            &Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
        )?;

        let heartbeat_file = douglas_folders.service_heartbeat_file(service_name);
        match permissions.change_user_and_group_ownership(
            &heartbeat_file,
            service_user,
            DOUGLAS_ADMIN_GROUP,
        ) {
            Ok(()) | Err(FileSystemError::NotFoundError(_)) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BootstrapError, DouglasService, State, Work, create_plan, liveness_check, require_liveness,
    };
    use std::path::PathBuf;

    use blueprint::{
        listener::LivenessCheck,
        service::{BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser},
    };
    fn service(name: &'static str, liveness: LivenessCheck) -> DouglasService {
        DouglasService {
            name,
            bootstrap_reporting: BootstrapReporting::Pipe,
            liveness: liveness.clone(),
            definition: ServiceDefinition::new(
                ServiceUser::create_managed(name),
                name,
                Vec::new(),
                &[],
                BootstrapReporting::Pipe,
                Some(liveness),
            ),
        }
    }

    fn tcp_liveness(port: u16) -> LivenessCheck {
        LivenessCheck::TcpPort {
            host: "127.0.0.1".to_string(),
            port,
        }
    }

    fn step_descriptions(steps: &[Box<dyn blueprint::Command<super::Context<'_>>>]) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_ensure_supervised_heartbeat_dirs_accessible_should_chown_every_supervised_service_to_the_admin_group()
     {
        let douglas_folders = config::DouglasFolders::new();
        let expected_paths = [
            (
                douglas_folders.heartbeat_dir(config::services::BRACT),
                credentials::ROOT_USER_NAME,
            ),
            (
                douglas_folders.service_heartbeat_file(config::services::BRACT),
                credentials::ROOT_USER_NAME,
            ),
            (
                douglas_folders.heartbeat_dir(resin::RESIN),
                resin::DOUGLAS_RESIN_USER,
            ),
            (
                douglas_folders.service_heartbeat_file(resin::RESIN),
                resin::DOUGLAS_RESIN_USER,
            ),
            (
                douglas_folders.heartbeat_dir(seedbank::SEEDBANK),
                seedbank::DOUGLAS_SEEDBANK_USER,
            ),
            (
                douglas_folders.service_heartbeat_file(seedbank::SEEDBANK),
                seedbank::DOUGLAS_SEEDBANK_USER,
            ),
        ];

        let mut permissions = file_system::MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .withf(move |path, user, group| {
                expected_paths.iter().any(|(expected_path, expected_user)| {
                    path == expected_path && user == *expected_user
                }) && group == credentials::well_known::DOUGLAS_ADMIN_GROUP
            })
            .times(6)
            .returning(|_, _, _| Ok(()));
        permissions
            .expect_change_mode()
            .times(3)
            .returning(|_, _| Ok(()));

        let result =
            super::ensure_supervised_heartbeat_dirs_accessible(&permissions, &douglas_folders);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_supervised_heartbeat_dirs_accessible_should_tolerate_a_missing_heartbeat_file() {
        let douglas_folders = config::DouglasFolders::new();
        let bract_heartbeat_file = douglas_folders.service_heartbeat_file(config::services::BRACT);

        let mut permissions = file_system::MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .withf(move |path, _, _| path == bract_heartbeat_file)
            .returning(|path, _, _| {
                Err(file_system::FileSystemError::NotFoundError(
                    path.to_path_buf(),
                ))
            });
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Ok(()));
        permissions.expect_change_mode().returning(|_, _| Ok(()));

        let result =
            super::ensure_supervised_heartbeat_dirs_accessible(&permissions, &douglas_folders);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_supervised_heartbeat_dirs_accessible_should_propagate_directory_errors() {
        let douglas_folders = config::DouglasFolders::new();

        let mut permissions = file_system::MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| {
                Err(file_system::FileSystemError::GroupNotFoundError(
                    credentials::well_known::DOUGLAS_ADMIN_GROUP.to_string(),
                ))
            });

        let result =
            super::ensure_supervised_heartbeat_dirs_accessible(&permissions, &douglas_folders);

        assert!(result.is_err());
    }

    #[test]
    fn test_create_plan_should_error_when_not_root() {
        let state = State::NotRoot;

        let result = create_plan(state);

        assert!(matches!(result, Err(BootstrapError::MustBeRoot)));
    }

    #[test]
    fn test_create_plan_should_produce_no_steps_when_nothing_is_needed() {
        let state = State::Root(Work::default());

        let Ok(steps) = create_plan(state) else {
            panic!("should plan");
        };

        assert!(steps.is_empty());
    }

    #[test]
    fn test_create_plan_should_create_the_cli_log_dir_when_missing() {
        let state = State::Root(Work {
            cli_log_dir_missing: Some(PathBuf::from("/var/log/douglas/douglas-cli")),
            ..Default::default()
        });

        let Ok(steps) = create_plan(state) else {
            panic!("should plan");
        };
        let descriptions = step_descriptions(&steps);

        assert_eq!(
            descriptions,
            vec!["Create folder '/var/log/douglas/douglas-cli'".to_string()]
        );
    }

    #[test]
    fn test_create_plan_should_batch_pipes_starts_and_waits_across_services() {
        let state = State::Root(Work {
            services_needing_start: vec![
                (service("bract", tcp_liveness(1)), ServiceState::default()),
                (service("resin", tcp_liveness(2)), ServiceState::default()),
            ],
            ..Default::default()
        });

        let Ok(steps) = create_plan(state) else {
            panic!("should plan");
        };
        let descriptions = step_descriptions(&steps);
        let Some(last_pipe) = descriptions
            .iter()
            .rposition(|d| d.starts_with("Create pipe for"))
        else {
            panic!("expected pipe steps");
        };
        let Some(first_start) = descriptions.iter().position(|d| d.starts_with("Start ")) else {
            panic!("expected start steps");
        };
        let Some(last_start) = descriptions.iter().rposition(|d| d.starts_with("Start ")) else {
            panic!("expected start steps");
        };
        let Some(first_wait) = descriptions.iter().position(|d| d.starts_with("Wait for")) else {
            panic!("expected wait steps");
        };

        assert!(last_pipe < first_start, "all pipes must precede all starts");
        assert!(last_start < first_wait, "all starts must precede all waits");
        assert_eq!(
            descriptions
                .iter()
                .filter(|d| d.starts_with("Start "))
                .count(),
            2
        );
    }

    #[test]
    fn test_require_liveness_should_return_the_configured_check() {
        let definition = ServiceDefinition::new(
            ServiceUser::create_managed("foo"),
            "foo",
            Vec::new(),
            &[],
            BootstrapReporting::Pipe,
            Some(tcp_liveness(1234)),
        );

        let result = require_liveness(&definition, "foo");

        assert!(matches!(
            result,
            Ok(LivenessCheck::TcpPort { port: 1234, .. })
        ));
    }

    #[test]
    fn test_require_liveness_should_error_when_none_is_configured() {
        let definition = ServiceDefinition::new(
            ServiceUser::create_managed("foo"),
            "foo",
            Vec::new(),
            &[],
            BootstrapReporting::Pipe,
            None,
        );

        let result = require_liveness(&definition, "foo");

        assert!(matches!(result, Err(BootstrapError::MissingLivenessCheck(name)) if name == "foo"));
    }

    #[test]
    fn test_liveness_check_should_resolve_bract_to_a_unix_socket() {
        let douglas_folders = config::DouglasFolders::new();

        let result = liveness_check(config::services::BRACT, &douglas_folders);

        assert!(matches!(result, Ok(LivenessCheck::UnixSocket(_))));
    }

    #[test]
    fn test_liveness_check_should_resolve_resin_to_a_tcp_port() {
        let douglas_folders = config::DouglasFolders::new();

        let result = liveness_check(config::services::RESIN, &douglas_folders);

        assert!(matches!(result, Ok(LivenessCheck::TcpPort { .. })));
    }

    #[test]
    fn test_liveness_check_should_reject_an_unknown_service() {
        let douglas_folders = config::DouglasFolders::new();

        let result = liveness_check("not-a-real-service", &douglas_folders);

        assert!(
            matches!(result, Err(BootstrapError::UnknownService(name)) if name == "not-a-real-service")
        );
    }
}
