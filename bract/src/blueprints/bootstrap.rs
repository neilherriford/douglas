use crate::BootstrapError;
use blueprint::{
    RunningStatus, Step,
    bootstrap::{build_boot_reporter, execute_plan, resolve_plan},
    listener::{ListenerDefinition, LivenessCheck, check_liveness},
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
use file_system::{Folder, Modes, Permissions};
use log::{Level, Reporter, ScopeKind, Span};
use std::sync::Arc;

pub async fn bootstrap(
    reporting_fd: i32,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
    docker_client_builder: &dyn ClientBuilder,
) -> Result<(), BootstrapError> {
    let boot_reporter = build_boot_reporter(
        douglas_folders.service_log_file("bract"),
        Some(reporting_fd),
    );

    bootstrap_with_reporter(
        boot_reporter,
        credentials,
        folder,
        permissions,
        douglas_folders,
        docker_client_builder,
    )
    .await
}

async fn bootstrap_with_reporter(
    boot_reporter: Arc<dyn Reporter>,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
    docker_client_builder: &dyn ClientBuilder,
) -> Result<(), BootstrapError> {
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
        let mut state_observer = StateObserver::new(credentials, docker_client.as_mut());
        state_observer
            .discover(guard.span(), &definition, credentials, folder, permissions)
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

    let plan = match resolve_plan(guard.span(), create_plan(&definition, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut context = Context::new(credentials, folder, permissions);
    let result = execute_plan(guard.span(), plan, &mut context, |reason| {
        BootstrapError::FailedBoostrap(vec![reason])
    })
    .await;

    if result.is_ok()
        && let Err(err) = ensure_trigger_socket_accessible(permissions, douglas_folders)
    {
        return guard.finish(Err(BootstrapError::FailedBoostrap(vec![err.to_string()])));
    }

    guard.finish(result)
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
                douglas_folders.socket_dir("bract"),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.socket_dir(reconcile_trigger_types::SOCKET_NAME),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.log_dir("bract"),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ],
        vec![
            ListenerDefinition::new(
                &douglas_folders.socket_file("bract"),
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

type Context<'a> = blueprint::StandardContext<'a>;

#[derive(Default)]
struct State {
    is_root: bool,
    bract_running_status: RunningStatus,
    docker_running_status: RunningStatus,
    service: ServiceState,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    docker_client: &'a mut dyn docker::client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        docker_client: &'a mut dyn docker::client::Client,
    ) -> Self {
        Self {
            credentials,
            docker_client,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        definition: &ServiceDefinition,
        credentials: &dyn Credentials,
        folder: &dyn Folder,
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

        result.service = discover_service_state(definition, credentials, folder, permissions)?;

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

    Ok(plan_service_bootstrap(definition, &state.service))
}

#[cfg(test)]
mod tests {
    use super::{bootstrap_with_reporter, service_definition};
    use crate::BootstrapError;
    use config::DouglasFolders;
    use credentials::{
        MockCredentials,
        well_known::{DOUGLAS_ADMIN_GROUP, DOUGLAS_RESIN_BRACT_GROUP},
    };
    use docker::{DockerError, MockClient, client::Client, client::MockClientBuilder};
    use file_system::{MockFolder, MockPermissions, Modes};
    use log::{Event, Reporter};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
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
                douglas_folders.socket_file("bract"),
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
            .find(|listener| listener.socket_path == douglas_folders.socket_file("bract"))
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
            &credentials,
            &MockFolder::new(),
            &MockPermissions::new(),
            &DouglasFolders::new(),
            &docker_client_builder,
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

        let result = bootstrap_with_reporter(
            Arc::new(NullReporter),
            &credentials,
            &MockFolder::new(),
            &MockPermissions::new(),
            &DouglasFolders::new(),
            &docker_client_builder,
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
}
