use crate::BootstrapError;
use async_trait::async_trait;
use blueprint::{
    Command, HasCredentials, HasFolder, HasPermissions, RunningStatus, Step,
    bootstrap::{build_boot_reporter, execute_plan, resolve_plan},
    commands::{CreateFolder, SetMode, SetOwnership},
    listener::{ListenerDefinition, LivenessCheck, check_liveness},
    push_step,
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser, discover_service_state,
        plan_service_bootstrap,
    },
};
use config::DouglasFolders;
use credentials::{
    Credentials,
    well_known::{DOUGLAS_ADMIN_GROUP, DOUGLAS_RESIN_BRACT_GROUP},
};
use docker::client::ClientBuilder;
use file_system::{
    FileDeleter, FileWriter, Folder, Inspect, Links, Modes, Permissions, path_to_string,
};
use log::{Level, Reporter, ScopeKind, Span};
use os::Os;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

pub(crate) use config::services::BRACT;

pub(crate) struct Dependencies<'a> {
    pub credentials: &'a dyn Credentials,
    pub folder: &'a dyn Folder,
    pub file_writer: &'a dyn FileWriter,
    pub file_deleter: &'a dyn FileDeleter,
    pub links: &'a dyn Links,
    pub permissions: &'a dyn Permissions,
    pub inspect: &'a dyn Inspect,
    pub os: &'a dyn Os,
    pub douglas_folders: &'a DouglasFolders,
    pub docker_client_builder: &'a dyn ClientBuilder,
}

pub async fn bootstrap(reporting_fd: i32, deps: Dependencies<'_>) -> Result<(), BootstrapError> {
    let boot_reporter = build_boot_reporter(
        deps.douglas_folders.service_log_file(BRACT),
        Some(reporting_fd),
    );

    bootstrap_with_reporter(boot_reporter, deps).await
}

async fn bootstrap_with_reporter(
    boot_reporter: Arc<dyn Reporter>,
    deps: Dependencies<'_>,
) -> Result<(), BootstrapError> {
    let Dependencies {
        credentials,
        folder,
        file_writer,
        file_deleter,
        links,
        permissions,
        inspect,
        os,
        douglas_folders,
        docker_client_builder,
    } = deps;

    let guard = Span::new(
        Arc::clone(&boot_reporter),
        "Bootstrapping douglas-bract system",
        log::ScopeKind::Group,
    )
    .start_guard();
    let definition = service_definition(douglas_folders);
    let mut docker_client = match docker_client_builder
        .build(Arc::clone(&boot_reporter))
        .await
    {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(BootstrapError::FailedBoostrap(vec![err.to_string()])));
        }
    };

    let state = {
        let mut state_observer = StateObserver {
            credentials,
            docker_client: docker_client.as_mut(),
        };
        state_observer
            .discover(guard.span(), &definition, folder, inspect, permissions)
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

    if state.docker_running_status == RunningStatus::Running
        && let Err(err) = ensure_system_network(docker_client.as_ref()).await
    {
        return guard.finish(Err(BootstrapError::FailedBoostrap(vec![err.to_string()])));
    }

    let binary_path = match os.current_executable() {
        Ok(path) => path,
        Err(err) => return guard.finish(Err(BootstrapError::from(err))),
    };

    let plan = match resolve_plan(
        guard.span(),
        create_plan(&definition, state, douglas_folders),
    ) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut context = Context {
        credentials,
        folder,
        file_writer,
        file_deleter,
        permissions,
        os,
    };
    let result = execute_plan(guard.span(), plan, &mut context, |reason| {
        BootstrapError::FailedBoostrap(vec![reason])
    })
    .await;

    if result.is_ok()
        && let Err(err) = ensure_binary_link(links, douglas_folders, &binary_path)
    {
        return guard.finish(Err(BootstrapError::FailedBoostrap(vec![err.to_string()])));
    }

    if result.is_ok()
        && let Err(err) = ensure_trigger_socket_accessible(permissions, douglas_folders)
    {
        return guard.finish(Err(BootstrapError::FailedBoostrap(vec![err.to_string()])));
    }

    guard.finish(result)
}

fn ensure_binary_link(
    links: &dyn Links,
    douglas_folders: &DouglasFolders,
    binary_path: &Path,
) -> Result<(), file_system::FileSystemError> {
    let link = douglas_folders.binary_link();

    if links.follow_symbolic(&link).ok().as_deref() == Some(binary_path) {
        return Ok(());
    }

    links.remove(&link)?;
    links.create_symbolic(binary_path, &link)
}

fn ensure_trigger_socket_accessible(
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
) -> Result<(), file_system::FileSystemError> {
    let trigger_socket_dir = douglas_folders.socket_dir(reconcile_trigger_types::SOCKET_NAME);
    permissions.change_user_and_group_ownership(
        &trigger_socket_dir,
        credentials::ROOT_USER_NAME,
        DOUGLAS_RESIN_BRACT_GROUP,
    )
}

pub fn service_definition(douglas_folders: &DouglasFolders) -> ServiceDefinition {
    ServiceDefinition::with_sockets(
        ServiceUser::create_system(credentials::ROOT_USER_NAME),
        DOUGLAS_ADMIN_GROUP,
        vec![
            (
                douglas_folders.logs.clone(),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.transients.clone(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.seedlings_root.clone(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.services(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.binary_dir(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.seedling_mounts(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.configs.clone(),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.rolodex(),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (douglas_folders.credentials(), Modes::OwnerReadWriteExecute),
            (
                douglas_folders.identity.clone(),
                Modes::OwnerReadWriteExecute,
            ),
            (
                douglas_folders.socket_dir(BRACT),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.socket_dir(reconcile_trigger_types::SOCKET_NAME),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.log_dir(BRACT),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.heartbeat_dir(BRACT),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ],
        vec![
            ListenerDefinition::new(
                &douglas_folders.socket_file(BRACT),
                credentials::ROOT_USER_NAME,
                DOUGLAS_ADMIN_GROUP,
                Modes::OwnerReadWriteGroupReadWrite,
            ),
            ListenerDefinition::new(
                &douglas_folders.socket_file(reconcile_trigger_types::SOCKET_NAME),
                credentials::ROOT_USER_NAME,
                DOUGLAS_RESIN_BRACT_GROUP,
                Modes::OwnerReadWriteGroupReadWrite,
            ),
        ],
        &[],
        BootstrapReporting::Pipe,
        Some(LivenessCheck::UnixSocket(
            douglas_folders.socket_file(BRACT),
        )),
    )
}

async fn ensure_system_network(
    docker_client: &dyn docker::client::Client,
) -> Result<(), docker::DockerError> {
    let network_name: docker_types::NetworkName = crate::blueprints::SYSTEM_NETWORK_NAME
        .parse()
        .expect("SYSTEM_NETWORK_NAME is a valid network name");

    if docker_client.network_exists(&network_name).await? {
        return Ok(());
    }

    docker_client.create_network(&network_name, None).await
}

fn sudoers_directory_path() -> PathBuf {
    PathBuf::from("/etc/sudoers.d")
}

fn sudoers_file_path() -> PathBuf {
    sudoers_directory_path().join("douglas-kick")
}

fn sudoers_rule(binary_path: &Path) -> String {
    let binary = binary_path.display();
    format!(
        "{woodward} ALL=(root) NOPASSWD: {binary} kick {bract}, {binary} kick {resin}, {binary} kick {seedbank}\n",
        woodward = config::services::WOODWARD,
        bract = config::services::BRACT,
        resin = config::services::RESIN,
        seedbank = config::services::SEEDBANK,
    )
}

struct Context<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
    file_writer: &'a dyn FileWriter,
    file_deleter: &'a dyn FileDeleter,
    permissions: &'a dyn Permissions,
    os: &'a dyn Os,
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
    service: ServiceState,
    sudoers_entry_exists: bool,
    sudoers_entry_has_correct_mode: bool,
    sudoers_entry_has_correct_ownership: bool,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    docker_client: &'a mut dyn docker::client::Client,
}

impl<'a> StateObserver<'a> {
    pub async fn discover(
        &mut self,
        span: &Span,
        definition: &ServiceDefinition,
        folder: &dyn Folder,
        inspect: &dyn Inspect,
        permissions: &dyn Permissions,
    ) -> Result<State, BootstrapError> {
        let guard = span
            .create_child(
                "Starting bract system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State {
            bract_running_status: self.check_bract_socket(guard.span(), definition),
            ..Default::default()
        };

        if result.bract_running_status == RunningStatus::Running {
            return guard.finish(Ok(result));
        }

        if !self.credentials.is_root() {
            return guard.finish(Ok(result));
        }
        result.is_root = true;
        result.docker_running_status = self.check_docker_running_status(guard.span()).await;
        if result.docker_running_status != RunningStatus::Running {
            return guard.finish(Ok(result));
        }

        result.service = discover_service_state(definition, self.credentials, folder, permissions)?;

        let sudoers_file = sudoers_file_path();
        if inspect.exists(&sudoers_file) {
            result.sudoers_entry_exists = true;
            result.sudoers_entry_has_correct_ownership = permissions
                .get_user_and_group_ownership(&sudoers_file)
                .is_ok_and(|(user, group)| {
                    user == credentials::ROOT_USER_NAME && group == credentials::ROOT_GROUP_NAME
                });
            result.sudoers_entry_has_correct_mode = permissions
                .get_mode(&sudoers_file)
                .is_ok_and(|mode| mode == Modes::OwnerReadGroupRead);
        }
        guard.finish(Ok(result))
    }

    fn check_bract_socket(&self, span: &Span, definition: &ServiceDefinition) -> RunningStatus {
        let Some(socket) = definition.owned_sockets.first() else {
            return RunningStatus::Unknown;
        };
        check_liveness(span, &LivenessCheck::UnixSocket(socket.socket_path.clone()))
    }

    async fn check_docker_running_status(&mut self, span: &Span) -> RunningStatus {
        match self.docker_client.ping().await {
            Ok(()) => RunningStatus::Running,
            Err(err) => {
                span.message(Level::Warn, &format!("Docker ping failed: '{err}'"));
                RunningStatus::Unknown
            }
        }
    }
}

fn create_plan<'a>(
    definition: &ServiceDefinition,
    state: State,
    douglas_folders: &DouglasFolders,
) -> Result<Vec<Step<Context<'a>>>, BootstrapError> {
    if state.bract_running_status == RunningStatus::Running {
        return Ok(Vec::new());
    }

    if state.docker_running_status != RunningStatus::Running {
        return Err(BootstrapError::MustHaveRunningDocker);
    }

    if !state.is_root {
        return Err(BootstrapError::MustBeRoot);
    }

    let mut steps = plan_service_bootstrap(definition, &state.service);

    push_step(&mut steps, CreateFolder::new(sudoers_directory_path()));
    push_step(
        &mut steps,
        SetOwnership::new(
            sudoers_directory_path(),
            credentials::ROOT_USER_NAME,
            credentials::ROOT_GROUP_NAME,
        ),
    );
    push_step(
        &mut steps,
        SetMode::new(sudoers_directory_path(), Modes::Other(0o755)),
    );

    if !state.sudoers_entry_exists {
        push_step(
            &mut steps,
            CreateSudoersFile::new(douglas_folders.binary_link()),
        );
    }
    push_step(&mut steps, ValidateSudoersFile::default());

    if !state.sudoers_entry_has_correct_ownership {
        push_step(
            &mut steps,
            SetOwnership::new(
                sudoers_file_path(),
                credentials::ROOT_USER_NAME,
                credentials::ROOT_GROUP_NAME,
            ),
        );
    }

    if !state.sudoers_entry_has_correct_mode {
        push_step(
            &mut steps,
            SetMode::new(sudoers_file_path(), Modes::OwnerReadGroupRead),
        );
    }

    Ok(steps)
}

#[derive(Debug)]
struct CreateSudoersFile {
    binary_link: PathBuf,
}

impl CreateSudoersFile {
    fn new(binary_link: PathBuf) -> Self {
        Self { binary_link }
    }
}

impl std::fmt::Display for CreateSudoersFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create sudoers file")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateSudoersFile {
    fn name(&self) -> String {
        "Creating sudoers file".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Creating sudoers file", ScopeKind::Step)
            .start_guard();

        context
            .file_writer
            .write_all(&sudoers_file_path(), &sudoers_rule(&self.binary_link))?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

const VISUDO_CANDIDATES: [&str; 5] = [
    "visudo",
    "/usr/sbin/visudo",
    "/sbin/visudo",
    "/usr/bin/visudo",
    "/run/current-system/sw/bin/visudo",
];

#[derive(Debug, Default)]
struct ValidateSudoersFile {}

#[derive(Error, Debug)]
enum ValidateSudoersFileError {
    #[error("OS error: {0}")]
    OsError(#[from] os::OsError),
    #[error("visudo could not be found")]
    CommandNotFound,
    #[error("visudo rejected the sudoers file (exit status {code:?})")]
    ValidationFailed { code: Option<i32> },
}

impl ValidateSudoersFile {
    fn validate(&self, os: &dyn Os) -> Result<(), ValidateSudoersFileError> {
        let sudoers_file = path_to_string(sudoers_file_path());
        for command in VISUDO_CANDIDATES {
            match os.execute(
                command,
                vec!["-c".to_string(), "-f".to_string(), sudoers_file.clone()],
                Vec::new(),
            ) {
                Ok(()) => return Ok(()),
                Err(os::OsError::IoError(io_error))
                    if io_error.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(os::OsError::ProccessExitStatusError { code, .. }) => {
                    return Err(ValidateSudoersFileError::ValidationFailed { code });
                }
                Err(err) => return Err(err.into()),
            }
        }

        Err(ValidateSudoersFileError::CommandNotFound)
    }
}

impl std::fmt::Display for ValidateSudoersFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Validate sudoers file")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ValidateSudoersFile {
    fn name(&self) -> String {
        "Validate sudoers file".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Validating sudoers file", ScopeKind::Step)
            .start_guard();

        match self.validate(context.os) {
            Ok(()) => guard.finish(Ok(())),
            Err(error @ ValidateSudoersFileError::ValidationFailed { .. }) => {
                if let Err(delete_error) = context.file_deleter.delete(&sudoers_file_path()) {
                    guard.span().message(
                        Level::Warn,
                        &format!("Could not remove the invalid sudoers file: {delete_error}"),
                    );
                }
                guard.finish(Err(Box::new(error)))
            }
            Err(error) => guard.finish(Err(Box::new(error))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BRACT, Context, CreateSudoersFile, Dependencies, State, VISUDO_CANDIDATES,
        ValidateSudoersFile, ValidateSudoersFileError, bootstrap_with_reporter, create_plan,
        ensure_binary_link, service_definition, sudoers_rule,
    };
    use crate::BootstrapError;
    use blueprint::{Command, RunningStatus, service::ServiceState};
    use config::DouglasFolders;
    use credentials::{
        MockCredentials,
        well_known::{DOUGLAS_ADMIN_GROUP, DOUGLAS_RESIN_BRACT_GROUP},
    };
    use docker::{DockerError, MockClient, client::Client, client::MockClientBuilder};
    use file_system::{
        MockFileDeleter, MockFileWriter, MockFolder, MockInspect, MockLinks, MockPermissions, Modes,
    };
    use log::{Event, Reporter, ScopeKind, Span};
    use os::MockOs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn test_span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    fn not_found_error() -> os::OsError {
        os::OsError::IoError(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    fn bootstrappable_state() -> State {
        State {
            is_root: true,
            bract_running_status: RunningStatus::NotRunning,
            docker_running_status: RunningStatus::Running,
            service: ServiceState::default(),
            sudoers_entry_exists: false,
            sudoers_entry_has_correct_mode: false,
            sudoers_entry_has_correct_ownership: false,
        }
    }

    #[test]
    fn test_service_definition_should_declare_both_the_main_and_trigger_sockets() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert_eq!(
            definition
                .owned_sockets
                .iter()
                .map(|listener| listener.socket_path.clone())
                .collect::<Vec<_>>(),
            vec![
                douglas_folders.socket_file(BRACT),
                douglas_folders.socket_file(reconcile_trigger_types::SOCKET_NAME),
            ]
        );
    }

    #[test]
    fn test_service_definition_should_own_the_main_socket_with_the_admin_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        let main_socket = definition
            .owned_sockets
            .iter()
            .find(|listener| listener.socket_path == douglas_folders.socket_file(BRACT))
            .expect("main socket should be declared");

        assert_eq!(main_socket.owning_group, DOUGLAS_ADMIN_GROUP);
    }

    #[test]
    fn test_service_definition_should_own_the_trigger_socket_with_the_resin_bract_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        let trigger_socket = definition
            .owned_sockets
            .iter()
            .find(|listener| {
                listener.socket_path
                    == douglas_folders.socket_file(reconcile_trigger_types::SOCKET_NAME)
            })
            .expect("trigger socket should be declared");

        assert_eq!(trigger_socket.owning_group, DOUGLAS_RESIN_BRACT_GROUP);
    }

    #[test]
    fn test_service_definition_should_own_the_trigger_socket_directory() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert!(definition.owned_folders.iter().any(|(path, _mode)| path
            == &douglas_folders.socket_dir(reconcile_trigger_types::SOCKET_NAME)));
    }

    #[test]
    fn test_service_definition_should_own_the_credentials_directory_as_owner_only() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert!(
            definition
                .owned_folders
                .iter()
                .any(|(path, mode)| path == &douglas_folders.credentials()
                    && mode == &Modes::OwnerReadWriteExecute)
        );
    }

    #[test]
    fn test_service_definition_should_own_the_identity_directory_as_owner_only() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert!(
            definition
                .owned_folders
                .iter()
                .any(|(path, mode)| path == &douglas_folders.identity
                    && mode == &Modes::OwnerReadWriteExecute)
        );
    }

    fn client_builder_returning(client: MockClient) -> MockClientBuilder {
        let mut builder = MockClientBuilder::new();
        builder
            .expect_build()
            .return_once(move |_reporter| Ok(Box::new(client) as Box<dyn Client>));
        builder
    }

    #[tokio::test]
    async fn test_bootstrap_should_fail_when_not_root() {
        let mut credentials = MockCredentials::new();
        credentials.expect_is_root().returning(|| false);

        let docker_client_builder = client_builder_returning(MockClient::new());

        let result = bootstrap_with_reporter(
            Arc::new(NullReporter),
            Dependencies {
                credentials: &credentials,
                folder: &MockFolder::new(),
                file_writer: &MockFileWriter::new(),
                file_deleter: &MockFileDeleter::new(),
                links: &MockLinks::new(),
                permissions: &MockPermissions::new(),
                inspect: &MockInspect::new(),
                os: &MockOs::new(),
                douglas_folders: &DouglasFolders::new(),
                docker_client_builder: &docker_client_builder,
            },
        )
        .await;

        assert!(matches!(result, Err(BootstrapError::MustBeRoot)));
    }

    #[tokio::test]
    async fn test_bootstrap_should_fail_when_docker_is_not_running() {
        let mut credentials = MockCredentials::new();
        credentials.expect_is_root().returning(|| true);

        let mut client = MockClient::new();
        client
            .expect_ping()
            .returning(|| Err(DockerError::PingFailed("connection refused".to_string())));

        let docker_client_builder = client_builder_returning(client);

        let mut os = MockOs::new();
        os.expect_current_executable()
            .returning(|| Ok(PathBuf::from("/usr/local/bin/douglas")));

        let result = bootstrap_with_reporter(
            Arc::new(NullReporter),
            Dependencies {
                credentials: &credentials,
                folder: &MockFolder::new(),
                file_writer: &MockFileWriter::new(),
                file_deleter: &MockFileDeleter::new(),
                links: &MockLinks::new(),
                permissions: &MockPermissions::new(),
                inspect: &MockInspect::new(),
                os: &os,
                douglas_folders: &DouglasFolders::new(),
                docker_client_builder: &docker_client_builder,
            },
        )
        .await;

        assert!(matches!(result, Err(BootstrapError::MustHaveRunningDocker)));
    }

    fn expected_network_name() -> docker_types::NetworkName {
        crate::blueprints::SYSTEM_NETWORK_NAME
            .parse()
            .expect("valid network name")
    }

    #[tokio::test]
    async fn test_ensure_system_network_should_create_it_when_missing() {
        let mut client = MockClient::new();
        client
            .expect_network_exists()
            .withf(|name| name == &expected_network_name())
            .returning(|_| Ok(false));
        client
            .expect_create_network()
            .withf(|name, subnet| name == &expected_network_name() && subnet.is_none())
            .returning(|_, _| Ok(()));

        let result = super::ensure_system_network(&client).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ensure_system_network_should_do_nothing_when_it_already_exists() {
        let mut client = MockClient::new();
        client
            .expect_network_exists()
            .withf(|name| name == &expected_network_name())
            .returning(|_| Ok(true));
        client.expect_create_network().never();

        let result = super::ensure_system_network(&client).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ensure_system_network_should_propagate_errors() {
        let mut client = MockClient::new();
        client
            .expect_network_exists()
            .returning(|_| Err(DockerError::PingFailed("connection refused".to_string())));

        let result = super::ensure_system_network(&client).await;

        assert!(result.is_err());
    }

    #[test]
    fn test_ensure_binary_link_should_do_nothing_when_it_already_points_at_the_binary() {
        let douglas_folders = DouglasFolders::new();
        let expected_link = douglas_folders.binary_link();
        let binary_path = PathBuf::from("/opt/douglas/bin/douglas");

        let mut links = MockLinks::new();
        let returned_target = binary_path.clone();
        links
            .expect_follow_symbolic()
            .withf(move |path| path == expected_link)
            .returning(move |_| Ok(returned_target.clone()));
        links.expect_remove().never();
        links.expect_create_symbolic().never();

        let result = ensure_binary_link(&links, &douglas_folders, &binary_path);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_binary_link_should_recreate_the_link_when_it_is_stale() {
        let douglas_folders = DouglasFolders::new();
        let expected_link = douglas_folders.binary_link();
        let binary_path = PathBuf::from("/opt/douglas/bin/douglas");

        let mut links = MockLinks::new();
        links
            .expect_follow_symbolic()
            .returning(|_| Ok(PathBuf::from("/old/store/path/douglas")));
        let link_for_remove = expected_link.clone();
        links
            .expect_remove()
            .withf(move |path| path == link_for_remove)
            .times(1)
            .returning(|_| Ok(()));
        let link_for_create = expected_link.clone();
        let target_for_create = binary_path.clone();
        links
            .expect_create_symbolic()
            .withf(move |from, to| from == target_for_create && to == link_for_create)
            .times(1)
            .returning(|_, _| Ok(()));

        let result = ensure_binary_link(&links, &douglas_folders, &binary_path);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_binary_link_should_create_the_link_when_it_is_missing() {
        let douglas_folders = DouglasFolders::new();
        let binary_path = PathBuf::from("/opt/douglas/bin/douglas");

        let mut links = MockLinks::new();
        links.expect_follow_symbolic().returning(|path| {
            Err(file_system::FileSystemError::NotFoundError(
                path.to_path_buf(),
            ))
        });
        links.expect_remove().times(1).returning(|_| Ok(()));
        links
            .expect_create_symbolic()
            .times(1)
            .returning(|_, _| Ok(()));

        let result = ensure_binary_link(&links, &douglas_folders, &binary_path);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_trigger_socket_dir_group_should_chown_it_to_the_resin_bract_group() {
        let douglas_folders = DouglasFolders::new();
        let expected_dir = douglas_folders.socket_dir(reconcile_trigger_types::SOCKET_NAME);

        let mut permissions = MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .withf(move |path, user, group| {
                path == expected_dir
                    && user == credentials::ROOT_USER_NAME
                    && group == DOUGLAS_RESIN_BRACT_GROUP
            })
            .returning(|_, _, _| Ok(()));

        let result = super::ensure_trigger_socket_accessible(&permissions, &douglas_folders);

        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_trigger_socket_dir_group_should_propagate_errors() {
        let douglas_folders = DouglasFolders::new();

        let mut permissions = MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| {
                Err(file_system::FileSystemError::NotFoundError(PathBuf::from(
                    "/run/douglas/bract-trigger",
                )))
            });

        let result = super::ensure_trigger_socket_accessible(&permissions, &douglas_folders);

        assert!(result.is_err());
    }

    #[test]
    fn test_sudoers_rule_should_grant_woodward_passwordless_kick_for_each_core_service() {
        let rule = sudoers_rule(Path::new("/opt/douglas/bin/douglas"));

        assert!(rule.starts_with("woodward ALL=(root) NOPASSWD: "));
        assert!(rule.contains("/opt/douglas/bin/douglas kick bract"));
        assert!(rule.contains("/opt/douglas/bin/douglas kick resin"));
        assert!(rule.contains("/opt/douglas/bin/douglas kick seedbank"));
        assert!(rule.ends_with('\n'));
    }

    fn step_descriptions(plan: &[super::Step<Context<'_>>]) -> Vec<String> {
        plan.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_create_validate_and_fix_the_sudoers_file_when_it_is_absent() {
        let douglas_folders = DouglasFolders::new();
        let definition = service_definition(&douglas_folders);

        let plan = create_plan(&definition, bootstrappable_state(), &douglas_folders)
            .expect("plan should resolve");

        let descriptions = step_descriptions(&plan);
        assert_eq!(descriptions[0], "Create folder '/etc/sudoers.d'");
        assert_eq!(
            descriptions[1],
            "Set ownership on '/etc/sudoers.d' to user 'root' group 'root'"
        );
        assert_eq!(descriptions[2], "Set mode on '/etc/sudoers.d' to '0o755'");
        assert_eq!(descriptions[3], "Create sudoers file");
        assert_eq!(descriptions[4], "Validate sudoers file");
        assert!(descriptions[5].starts_with("Set ownership on '/etc/sudoers.d/douglas-kick'"));
        assert!(descriptions[6].starts_with("Set mode on '/etc/sudoers.d/douglas-kick'"));
        assert_eq!(descriptions.len(), 7);
    }

    #[test]
    fn test_create_plan_should_ensure_the_directory_and_validate_when_the_file_is_present_and_correct()
     {
        let douglas_folders = DouglasFolders::new();
        let definition = service_definition(&douglas_folders);

        let state = State {
            sudoers_entry_exists: true,
            sudoers_entry_has_correct_mode: true,
            sudoers_entry_has_correct_ownership: true,
            ..bootstrappable_state()
        };

        let plan = create_plan(&definition, state, &douglas_folders).expect("plan should resolve");

        assert_eq!(
            step_descriptions(&plan),
            vec![
                "Create folder '/etc/sudoers.d'",
                "Set ownership on '/etc/sudoers.d' to user 'root' group 'root'",
                "Set mode on '/etc/sudoers.d' to '0o755'",
                "Validate sudoers file",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_be_empty_when_bract_is_already_running() {
        let douglas_folders = DouglasFolders::new();
        let definition = service_definition(&douglas_folders);

        let state = State {
            bract_running_status: RunningStatus::Running,
            ..bootstrappable_state()
        };

        let plan = create_plan(&definition, state, &douglas_folders).expect("plan should resolve");

        assert!(plan.is_empty());
    }

    #[test]
    fn test_create_plan_should_error_when_not_root() {
        let douglas_folders = DouglasFolders::new();
        let definition = service_definition(&douglas_folders);

        let state = State {
            is_root: false,
            ..bootstrappable_state()
        };

        let result = create_plan(&definition, state, &douglas_folders);

        assert!(matches!(result, Err(BootstrapError::MustBeRoot)));
    }

    #[test]
    fn test_create_plan_should_error_when_docker_is_not_running() {
        let douglas_folders = DouglasFolders::new();
        let definition = service_definition(&douglas_folders);

        let state = State {
            docker_running_status: RunningStatus::NotRunning,
            ..bootstrappable_state()
        };

        let result = create_plan(&definition, state, &douglas_folders);

        assert!(matches!(result, Err(BootstrapError::MustHaveRunningDocker)));
    }

    #[test]
    fn test_validate_should_succeed_when_visudo_accepts_the_file() {
        let mut os = MockOs::new();
        os.expect_execute()
            .withf(|command, args, env| {
                command == "visudo"
                    && args.as_slice() == ["-c", "-f", "/etc/sudoers.d/douglas-kick"]
                    && env.is_empty()
            })
            .times(1)
            .returning(|_, _, _| Ok(()));

        assert!(ValidateSudoersFile::default().validate(&os).is_ok());
    }

    #[test]
    fn test_validate_should_fall_through_to_a_later_candidate_when_earlier_ones_are_missing() {
        let mut os = MockOs::new();
        os.expect_execute().times(3).returning(|command, _, _| {
            if command == "/sbin/visudo" {
                Ok(())
            } else {
                Err(not_found_error())
            }
        });

        assert!(ValidateSudoersFile::default().validate(&os).is_ok());
    }

    #[test]
    fn test_validate_should_report_command_not_found_when_no_candidate_exists() {
        let mut os = MockOs::new();
        os.expect_execute()
            .times(VISUDO_CANDIDATES.len())
            .returning(|_, _, _| Err(not_found_error()));

        assert!(matches!(
            ValidateSudoersFile::default().validate(&os),
            Err(ValidateSudoersFileError::CommandNotFound)
        ));
    }

    #[test]
    fn test_validate_should_report_validation_failed_when_visudo_rejects_the_file() {
        let mut os = MockOs::new();
        os.expect_execute().times(1).returning(|_, _, _| {
            Err(os::OsError::ProccessExitStatusError {
                name: "visudo".to_string(),
                code: Some(1),
                args: Vec::new(),
            })
        });

        assert!(matches!(
            ValidateSudoersFile::default().validate(&os),
            Err(ValidateSudoersFileError::ValidationFailed { code: Some(1) })
        ));
    }

    #[test]
    fn test_validate_should_propagate_other_os_errors() {
        let mut os = MockOs::new();
        os.expect_execute()
            .times(1)
            .returning(|_, _, _| Err(os::OsError::PidTooLarge));

        assert!(matches!(
            ValidateSudoersFile::default().validate(&os),
            Err(ValidateSudoersFileError::OsError(_))
        ));
    }

    #[tokio::test]
    async fn test_validate_run_should_delete_the_file_and_fail_when_validation_fails() {
        let mut os = MockOs::new();
        os.expect_execute().returning(|_, _, _| {
            Err(os::OsError::ProccessExitStatusError {
                name: "visudo".to_string(),
                code: Some(1),
                args: Vec::new(),
            })
        });

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_delete()
            .withf(|path| path == Path::new("/etc/sudoers.d/douglas-kick"))
            .times(1)
            .returning(|_| Ok(()));

        let credentials = MockCredentials::new();
        let folder = MockFolder::new();
        let file_writer = MockFileWriter::new();
        let permissions = MockPermissions::new();
        let mut context = Context {
            credentials: &credentials,
            folder: &folder,
            file_writer: &file_writer,
            file_deleter: &file_deleter,
            permissions: &permissions,
            os: &os,
        };

        let span = test_span();
        let result = ValidateSudoersFile::default()
            .run(&span, &mut context)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_run_should_leave_the_file_alone_when_validation_passes() {
        let mut os = MockOs::new();
        os.expect_execute().returning(|_, _, _| Ok(()));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_delete().never();

        let credentials = MockCredentials::new();
        let folder = MockFolder::new();
        let file_writer = MockFileWriter::new();
        let permissions = MockPermissions::new();
        let mut context = Context {
            credentials: &credentials,
            folder: &folder,
            file_writer: &file_writer,
            file_deleter: &file_deleter,
            permissions: &permissions,
            os: &os,
        };

        let span = test_span();
        let result = ValidateSudoersFile::default()
            .run(&span, &mut context)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_run_should_fail_without_deleting_when_visudo_is_missing() {
        let mut os = MockOs::new();
        os.expect_execute()
            .returning(|_, _, _| Err(not_found_error()));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_delete().never();

        let credentials = MockCredentials::new();
        let folder = MockFolder::new();
        let file_writer = MockFileWriter::new();
        let permissions = MockPermissions::new();
        let mut context = Context {
            credentials: &credentials,
            folder: &folder,
            file_writer: &file_writer,
            file_deleter: &file_deleter,
            permissions: &permissions,
            os: &os,
        };

        let span = test_span();
        let result = ValidateSudoersFile::default()
            .run(&span, &mut context)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_sudoers_file_run_should_write_the_kick_rule() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .withf(|path, contents| {
                path == Path::new("/etc/sudoers.d/douglas-kick")
                    && contents.starts_with("woodward ALL=(root) NOPASSWD: ")
                    && contents.contains("/usr/local/bin/douglas kick bract")
                    && contents.contains("/usr/local/bin/douglas kick resin")
                    && contents.contains("/usr/local/bin/douglas kick seedbank")
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let credentials = MockCredentials::new();
        let folder = MockFolder::new();
        let file_deleter = MockFileDeleter::new();
        let permissions = MockPermissions::new();
        let os = MockOs::new();
        let mut context = Context {
            credentials: &credentials,
            folder: &folder,
            file_writer: &file_writer,
            file_deleter: &file_deleter,
            permissions: &permissions,
            os: &os,
        };

        let span = test_span();
        let result = CreateSudoersFile::new(PathBuf::from("/usr/local/bin/douglas"))
            .run(&span, &mut context)
            .await;

        assert!(result.is_ok());
    }
}
