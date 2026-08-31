use crate::{
    blueprints::{EXPECTED_MOUNT_MODE, agent_container_name, container_name},
    labels,
    rolodex::{Rolodex, RolodexError},
};
use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use config::DouglasFolders;
use docker::client::{ClientBuilder, ContainerRef, ImageRef};
use docker_types::{ContainerName, DockerNameError};
use file_system::{FileReader, FileSystemError, Inspect, Permissions};
use log::{Reporter, ScopeKind, Span};
use seedbank_types::MountContents;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StartSeedlingError {
    #[error("Resin error: {0}")]
    ResinError(#[from] resin_client::Error),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Rolodex error {0}")]
    RolodexError(#[from] RolodexError),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Seedbank error {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Cannot start seedling {0}")]
    CannotStartSeedling(String),
    #[error("Cannot start seedling {0}: it is a core seedling managed by douglas")]
    CoreSeedling(String),
}

struct Context<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

#[derive(Debug, Default)]
struct State {
    seedling_exists: bool,
    seedling_credentials_exist: bool,
    image_exists: bool,
    container_exists: bool,
    mounts_initialized: bool,
    container_is_startable: bool,
    container_name: Option<docker_types::ContainerName>,
    version: Option<seedbank_types::Version>,
    origin: Option<labels::Origin>,
    agent_container_exists: bool,
    agent_container_is_running: bool,
    agent_container_name: Option<docker_types::ContainerName>,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    inspect: &dyn Inspect,
    file_reader: &dyn FileReader,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
    docker_client_builder: &dyn ClientBuilder,
    seedbank_client: &dyn seedbank_client::Client,
    rolodex: &dyn Rolodex,
    registry: &docker_types::Registry,
    name: &seedbank_types::Name,
) -> Result<(), StartSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Starting seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut docker_client = match docker_client_builder.build(Arc::clone(&reporter)).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(StartSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let state = {
        let mut state_observer = StateObserver::new(
            &mut *docker_client,
            seedbank_client,
            rolodex,
            douglas_folders,
            inspect,
            file_reader,
            permissions,
            registry,
        );
        state_observer.discover(guard.span(), name).await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(name, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            docker_client: &mut *docker_client,
            seedbank_client,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            StartSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    rolodex: &'a dyn Rolodex,
    douglas_folders: &'a DouglasFolders,
    inspect: &'a dyn Inspect,
    file_reader: &'a dyn FileReader,
    permissions: &'a dyn Permissions,
    registry: &'a docker_types::Registry,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a mut dyn docker::client::Client,
        seedbank_client: &'a dyn seedbank_client::Client,
        rolodex: &'a dyn Rolodex,
        douglas_folders: &'a DouglasFolders,
        inspect: &'a dyn Inspect,
        file_reader: &'a dyn FileReader,
        permissions: &'a dyn Permissions,
        registry: &'a docker_types::Registry,
    ) -> Self {
        Self {
            docker_client,
            seedbank_client,
            rolodex,
            douglas_folders,
            inspect,
            file_reader,
            permissions,
            registry,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        name: &seedbank_types::Name,
    ) -> Result<State, StartSeedlingError> {
        let guard = span
            .create_child(
                "Starting seedling, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State::default();

        let agent_container = agent_container_name(name)?;
        result.agent_container_name = Some(agent_container.clone());
        result.agent_container_exists = self
            .docker_client
            .container_exists(ContainerRef::FullName(agent_container.clone()))
            .await?;
        if result.agent_container_exists {
            result.agent_container_is_running = self
                .docker_client
                .container_status(ContainerRef::FullName(agent_container))
                .await?
                == docker_types::Status::Running;
        }

        if !self.seedbank_client.exists(name).await? {
            return Ok(result);
        }
        result.seedling_exists = true;

        let Some(service_account) = self.rolodex.find_service_account(name.as_ref())? else {
            return Ok(result);
        };
        result.seedling_credentials_exist = true;

        let seedling = self.seedbank_client.load(name).await?;
        result.version = Some(seedling.version.clone());

        if !self
            .docker_client
            .image_exists(
                self.registry,
                ImageRef::VersionedName(seedling.definition.image.clone()),
            )
            .await?
        {
            return Ok(result);
        }
        result.image_exists = true;

        let derived_container_name: ContainerName = container_name(name)?;
        result.container_name = Some(derived_container_name.clone());
        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(derived_container_name.clone()))
            .await?
        {
            return Ok(result);
        }
        result.container_exists = true;

        let container_labels = self
            .docker_client
            .container_labels(ContainerRef::FullName(derived_container_name.clone()))
            .await?;
        result.origin = labels::get_origin(&container_labels);

        for (mount_name, mount_definition) in seedling.definition.mounts {
            let expected = self
                .douglas_folders
                .seedling_mount(seedling.name.as_ref(), mount_name.as_ref());

            if !self.inspect.exists(&expected) {
                return Ok(result);
            }

            let (owning_user, owning_group) =
                self.permissions.get_user_and_group_ownership(&expected)?;

            if service_account.user.system_name != owning_user
                || service_account.group.system_name != owning_group
            {
                return Ok(result);
            }

            if self.permissions.get_mode(&expected)? != EXPECTED_MOUNT_MODE {
                return Ok(result);
            }

            for content in mount_definition.contents() {
                match content {
                    MountContents::FolderOnly(relative_path) => {
                        let mut expected = expected.clone();
                        expected.push(relative_path);
                        if !self.inspect.exists(&expected) {
                            return Ok(result);
                        }
                    }
                    MountContents::File(mount_file) => {
                        let mut expected = expected.clone();
                        expected.push(mount_file.file_relative_path.clone());
                        if !self.inspect.exists(&expected) {
                            return Ok(result);
                        }
                        let actual_bytes = self.file_reader.read_all_bytes(&expected)?;

                        if mount_file.contents != actual_bytes {
                            return Ok(result);
                        }
                    }
                }
            }
        }
        result.mounts_initialized = true;

        result.container_is_startable = self
            .docker_client
            .container_status(ContainerRef::FullName(derived_container_name.clone()))
            .await?
            != docker_types::Status::Running;

        guard.finish(Ok(result))
    }
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    state: State,
) -> Result<Vec<Step<'a>>, StartSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

    if !state.seedling_exists {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Seedling not defined".to_string(),
        ));
    }
    if !state.seedling_credentials_exist {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Seedling credentials not created yet".to_string(),
        ));
    }
    if !state.image_exists {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Docker instance not initialized".to_string(),
        ));
    }
    if !state.container_exists {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Docker instance not initialized".to_string(),
        ));
    }
    if state.origin == Some(labels::Origin::Core) {
        return Err(StartSeedlingError::CoreSeedling(name.to_string()));
    }
    if !state.mounts_initialized {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Could not initialize".to_string(),
        ));
    }
    if !state.container_is_startable {
        return Err(StartSeedlingError::CannotStartSeedling(
            "The seedling is already running".to_string(),
        ));
    }

    let version = state
        .version
        .ok_or(StartSeedlingError::CannotStartSeedling(
            "Docker instance not initialized".to_string(),
        ))?;

    if state.agent_container_exists
        && !state.agent_container_is_running
        && let Some(agent_container_name) = state.agent_container_name
    {
        push_step(
            &mut steps,
            StartSeedling::new(name.clone(), agent_container_name, version.clone()),
        );
    }

    push_step(
        &mut steps,
        StartSeedling::new(
            name.clone(),
            state
                .container_name
                .ok_or(StartSeedlingError::CannotStartSeedling(
                    "Docker instance not initialized".to_string(),
                ))?,
            version,
        ),
    );

    push_step(&mut steps, SetDesiredRunStatusToRunning::new(name.clone()));
    Ok(steps)
}

struct StartSeedling {
    seedling_name: seedbank_types::Name,
    container_name: docker_types::ContainerName,
    version: seedbank_types::Version,
}

impl StartSeedling {
    pub fn new(
        seedling_name: seedbank_types::Name,
        container_name: docker_types::ContainerName,
        version: seedbank_types::Version,
    ) -> Self {
        Self {
            seedling_name,
            container_name,
            version,
        }
    }
}

impl std::fmt::Display for StartSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Starting seedling '{}' (v{})",
            self.seedling_name, self.version
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartSeedling {
    fn name(&self) -> String {
        "Starting seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Starting seedling '{}' (v{})",
                    self.seedling_name, self.version
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client
            .start_container(ContainerRef::FullName(self.container_name.clone()))
            .await?;

        guard.finish(Ok(()))
    }
}

struct SetDesiredRunStatusToRunning {
    seedling_name: seedbank_types::Name,
}

impl SetDesiredRunStatusToRunning {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for SetDesiredRunStatusToRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Setting seedling '{}' desired running status to running",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for SetDesiredRunStatusToRunning {
    fn name(&self) -> String {
        "Setting desired running state to running".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Setting seedling '{}' desired running status to running",
                    self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .set_desired_run_status(
                &self.seedling_name,
                seedbank_types::DesiredRunStatus::Running,
            )
            .await?;
        guard.finish(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "traefik".parse().unwrap()
    }

    fn startable_state() -> State {
        State {
            seedling_exists: true,
            seedling_credentials_exist: true,
            image_exists: true,
            container_exists: true,
            mounts_initialized: true,
            container_is_startable: true,
            container_name: Some(container_name(&name()).unwrap()),
            version: Some(seedbank_types::Version(1)),
            origin: Some(labels::Origin::User),
            agent_container_exists: false,
            agent_container_is_running: false,
            agent_container_name: Some(agent_container_name(&name()).unwrap()),
        }
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_start_a_startable_container() {
        let steps = create_plan(&name(), startable_state()).expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_seedling_is_not_defined() {
        let result = create_plan(
            &name(),
            State {
                seedling_exists: false,
                ..startable_state()
            },
        );

        assert!(matches!(
            result,
            Err(StartSeedlingError::CannotStartSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_when_credentials_are_missing() {
        let result = create_plan(
            &name(),
            State {
                seedling_credentials_exist: false,
                ..startable_state()
            },
        );

        assert!(matches!(
            result,
            Err(StartSeedlingError::CannotStartSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_when_mounts_are_not_initialized() {
        let result = create_plan(
            &name(),
            State {
                mounts_initialized: false,
                ..startable_state()
            },
        );

        assert!(matches!(
            result,
            Err(StartSeedlingError::CannotStartSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_when_already_running() {
        let result = create_plan(
            &name(),
            State {
                container_is_startable: false,
                ..startable_state()
            },
        );

        assert!(matches!(
            result,
            Err(StartSeedlingError::CannotStartSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_to_start_a_core_seedling() {
        let result = create_plan(
            &name(),
            State {
                origin: Some(labels::Origin::Core),
                ..startable_state()
            },
        );

        assert!(matches!(result, Err(StartSeedlingError::CoreSeedling(_))));
    }

    #[test]
    fn test_create_plan_should_start_a_stopped_agent_container_before_the_app() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_running: false,
                ..startable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Starting seedling 'traefik' (v1)",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_agent_when_it_does_not_exist() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: false,
                agent_container_is_running: false,
                ..startable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_agent_when_already_running() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_running: true,
                ..startable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }
}
