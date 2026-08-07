use crate::BootstrapError;
use blueprint::{
    Command, RunningStatus,
    bootstrap::{build_boot_reporter, execute_plan, resolve_plan},
    listener::{ListenerDefinition, LivenessCheck, check_liveness},
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser, discover_service_state,
        plan_service_bootstrap,
    },
};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
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

    let plan = match resolve_plan(guard.span(), create_plan(&definition, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut context = Context::new(credentials, folder, permissions);
    let result = execute_plan(guard.span(), plan, &mut context, |reason| {
        BootstrapError::FailedBoostrap(vec![reason])
    })
    .await;

    guard.finish(result)
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
                douglas_folders.applications.clone(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.application_services.clone(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.application_mounts.clone(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.configs.clone(),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.rolodex.clone(),
                Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.socket_dir("bract"),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.log_dir("bract"),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ],
        vec![ListenerDefinition::new(
            &douglas_folders.socket_file("bract"),
            credentials::ROOT_USER_NAME,
            DOUGLAS_ADMIN_GROUP,
            Modes::OwnerReadWriteGroupReadWrite,
        )],
        &[],
        BootstrapReporting::Pipe,
    )
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

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn create_plan<'a>(
    definition: &ServiceDefinition,
    state: State,
) -> Result<Vec<Step<'a>>, BootstrapError> {
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
    use super::bootstrap_with_reporter;
    use crate::BootstrapError;
    use config::DouglasFolders;
    use credentials::MockCredentials;
    use docker::{DockerError, MockClient, client::Client, client::MockClientBuilder};
    use file_system::{MockFolder, MockPermissions};
    use log::{Event, Reporter};
    use std::sync::Arc;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
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
}
