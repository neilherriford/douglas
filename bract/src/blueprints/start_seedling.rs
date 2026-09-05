use crate::{
    blueprints::{
        EXPECTED_MOUNT_MODE, RequestedBy, agent_container_name, container_name,
        core_seedling_forbidden_for, provision_seedling_secrets,
    },
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
use docker_types::{ContainerName, DockerNameError, ExecInstanceOptions, ExecStartOptions};
use file_system::{FileReader, FileSystemError, Inspect, Permissions};
use log::{Reporter, ScopeKind, Span};
use seedbank_types::MountContents;
use std::sync::Arc;
use thiserror::Error;

const EXIT_CODE_SUCCESS: i32 = 0;
const HEALTH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const HEALTH_CHECK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

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
    #[error("Seedling start failed")]
    FailedToStart,
}

struct Context<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

#[derive(Debug, Default)]
struct State {
    seedling_exists: bool,
    seedling_credentials_exist: bool,
    reached_max_fail_count: bool,
    has_health_check_failure: bool,
    image_exists: bool,
    container_exists: bool,
    mounts_initialized: bool,
    container_is_startable: bool,
    container_name: Option<docker_types::ContainerName>,
    version: Option<seedbank_types::Version>,
    origin: Option<seedbank_types::Origin>,
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
    requested_by: RequestedBy,
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
            requested_by,
        );
        state_observer.discover(guard.span(), name).await?
    };

    let seedling = seedbank_client.load(name).await?;
    let (_, agent_ip) = provision_seedling_secrets::agent_private_network(&seedling.id);

    let plan = match resolve_plan(
        guard.span(),
        create_plan(
            name,
            &seedling.definition.health_check,
            agent_ip,
            state,
            requested_by,
        ),
    ) {
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
    requested_by: RequestedBy,
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
        requested_by: RequestedBy,
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
            requested_by,
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

        let health_check_log = self.seedbank_client.health_check_log(name).await?;
        result.has_health_check_failure = health_check_log.is_some();

        result.reached_max_fail_count = match self.requested_by {
            RequestedBy::Operator => false,
            RequestedBy::Watchdog => health_check_log
                .map(|log| log.reached_max_fail_count())
                .unwrap_or(false),
        };

        if result.reached_max_fail_count {
            return Ok(result);
        }

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
    health_check: &seedbank_types::HealthCheck,
    agent_ip: std::net::Ipv4Addr,
    state: State,
    requested_by: RequestedBy,
) -> Result<Vec<Step<'a>>, StartSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

    if !state.seedling_exists {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Seedling not defined".to_string(),
        ));
    }

    if state.reached_max_fail_count {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Seedling has failed its health checks".to_string(),
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

    if core_seedling_forbidden_for(state.origin, requested_by) {
        return Err(StartSeedlingError::CoreSeedling(name.to_string()));
    }

    if !state.mounts_initialized {
        return Err(StartSeedlingError::CannotStartSeedling(
            "Could not initialize".to_string(),
        ));
    }
    if !state.container_is_startable && !state.has_health_check_failure {
        return Err(StartSeedlingError::CannotStartSeedling(
            "The seedling is already running".to_string(),
        ));
    }
    let already_running = !state.container_is_startable;

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
            StartAgentContainer::new(agent_container_name, agent_ip),
        );
    }

    push_step(
        &mut steps,
        StartSeedling::new(
            name.clone(),
            health_check.clone(),
            state
                .container_name
                .ok_or(StartSeedlingError::CannotStartSeedling(
                    "Docker instance not initialized".to_string(),
                ))?,
            version,
            already_running,
        ),
    );

    push_step(&mut steps, ClearHealthCheckLogs::new(name.clone()));
    push_step(&mut steps, SetDesiredRunStatusToRunning::new(name.clone()));
    Ok(steps)
}

async fn start_container(
    context: &mut Context<'_>,
    container_name: &docker_types::ContainerName,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    context
        .docker_client
        .start_container(ContainerRef::FullName(container_name.clone()))
        .await?;
    Ok(())
}

async fn container_is_running(
    context: &mut Context<'_>,
    container_name: &docker_types::ContainerName,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(matches!(
        context
            .docker_client
            .container_status(ContainerRef::FullName(container_name.clone()))
            .await?,
        docker_types::Status::Running
    ))
}

fn shell_exec_instance_options(command: &str) -> ExecInstanceOptions {
    ExecInstanceOptions {
        attach_stdin: false,
        attach_stdout: true,
        attach_stderr: true,
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()],
    }
}

pub(crate) async fn run_shell_health_check(
    span: &Span,
    docker_client: &dyn docker::client::Client,
    container_ref: ContainerRef,
    command: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    span.message(
        log::Level::Info,
        &format!("Running health check command: {command}"),
    );

    let id = docker_client
        .create_exec_instance(&container_ref, &shell_exec_instance_options(command))
        .await?;
    docker_client
        .start_exec_instance(&id, &ExecStartOptions::default())
        .await?;

    let poll = async {
        loop {
            let result = docker_client.inspect_exec_instance(&id).await?;
            if !result.running {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result.exit_code);
            }
            tokio::time::sleep(HEALTH_CHECK_POLL_INTERVAL).await;
        }
    };

    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, poll).await {
        Ok(exit_code_result) => match exit_code_result? {
            Some(EXIT_CODE_SUCCESS) => {
                span.message(log::Level::Info, "Health check passed (exit code 0)");
                Ok(true)
            }
            Some(exit_code) => {
                span.message(
                    log::Level::Warn,
                    &format!("Health check failed with exit code {exit_code}"),
                );
                Ok(false)
            }
            None => {
                span.message(
                    log::Level::Warn,
                    "Health check finished without reporting an exit code",
                );
                Ok(false)
            }
        },
        Err(_elapsed) => {
            span.message(
                log::Level::Warn,
                &format!("Health check timed out after {HEALTH_CHECK_TIMEOUT:?}"),
            );
            Ok(false)
        }
    }
}

struct StartAgentContainer {
    container_name: docker_types::ContainerName,
    agent_ip: std::net::Ipv4Addr,
}

impl StartAgentContainer {
    pub fn new(container_name: docker_types::ContainerName, agent_ip: std::net::Ipv4Addr) -> Self {
        Self {
            container_name,
            agent_ip,
        }
    }

    fn health_check_command(&self) -> String {
        format!(
            "BAO_ADDR=http://{}:{} bao token lookup",
            self.agent_ip,
            openbao::AGENT_LOCAL_PROXY_PORT
        )
    }
}

impl std::fmt::Display for StartAgentContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Starting agent container '{}'", self.container_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartAgentContainer {
    fn name(&self) -> String {
        "Starting agent container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Starting agent container '{}'…", self.container_name),
                ScopeKind::Step,
            )
            .start_guard();

        start_container(context, &self.container_name).await?;
        guard.span().message(
            log::Level::Info,
            &format!("Agent container '{}' start issued", self.container_name),
        );

        let running = container_is_running(context, &self.container_name).await?;
        if !running {
            guard.span().message(
                log::Level::Warn,
                &format!(
                    "Agent container '{}' is not running after start",
                    self.container_name
                ),
            );
        }

        let is_healthy = running
            && run_shell_health_check(
                guard.span(),
                context.docker_client,
                ContainerRef::FullName(self.container_name.clone()),
                &self.health_check_command(),
            )
            .await?;

        if !is_healthy {
            guard.span().message(
                log::Level::Warn,
                &format!(
                    "Agent container '{}' failed to become healthy",
                    self.container_name
                ),
            );
            return guard.finish(Err(Box::new(StartSeedlingError::FailedToStart)));
        }

        guard.finish(Ok(()))
    }
}

struct StartSeedling {
    seedling_name: seedbank_types::Name,
    container_name: docker_types::ContainerName,
    version: seedbank_types::Version,
    health_check: seedbank_types::HealthCheck,
    already_running: bool,
}

impl StartSeedling {
    pub fn new(
        seedling_name: seedbank_types::Name,
        health_check: seedbank_types::HealthCheck,
        container_name: docker_types::ContainerName,
        version: seedbank_types::Version,
        already_running: bool,
    ) -> Self {
        Self {
            seedling_name,
            health_check,
            container_name,
            version,
            already_running,
        }
    }

    fn container_ref(&self) -> ContainerRef {
        ContainerRef::FullName(self.container_name.clone())
    }

    async fn health_check(
        &self,
        span: &Span,
        context: &Context<'_>,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        run_shell_health_check(
            span,
            context.docker_client,
            self.container_ref(),
            &self.health_check.command.to_string(),
        )
        .await
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

        if self.already_running {
            guard.span().message(
                log::Level::Info,
                &format!(
                    "Container '{}' is already running, rechecking its health",
                    self.container_name
                ),
            );
        } else {
            start_container(context, &self.container_name).await?;
            guard.span().message(
                log::Level::Info,
                &format!(
                    "Container '{}' start issued, waiting {}s before health check",
                    self.container_name, self.health_check.wait_time_in_seconds
                ),
            );

            tokio::time::sleep(std::time::Duration::from_secs(
                self.health_check.wait_time_in_seconds.get().into(),
            ))
            .await;
        }

        let running = container_is_running(context, &self.container_name).await?;
        if !running {
            guard.span().message(
                log::Level::Warn,
                &format!(
                    "Container '{}' is not running after start",
                    self.container_name
                ),
            );
        }

        let is_healthy = running && self.health_check(guard.span(), context).await?;

        if is_healthy {
            return guard.finish(Ok(()));
        }

        record_health_check_failure(
            guard.span(),
            context.docker_client,
            context.seedbank_client,
            &self.seedling_name,
            self.container_ref(),
        )
        .await?;

        guard.finish(Err(Box::new(StartSeedlingError::FailedToStart)))
    }
}

pub(crate) async fn record_health_check_failure(
    span: &Span,
    docker_client: &dyn docker::client::Client,
    seedbank_client: &dyn seedbank_client::Client,
    seedling_name: &seedbank_types::Name,
    container_ref: ContainerRef,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let reached_max_fail_count = seedbank_client
        .increment_health_log_fail_count(seedling_name)
        .await?;
    span.message(
        log::Level::Warn,
        &format!(
            "Seedling '{seedling_name}' failed its health check (reached_max_fail_count={reached_max_fail_count})"
        ),
    );

    if reached_max_fail_count {
        span.message(
            log::Level::Warn,
            &format!(
                "Seedling '{seedling_name}' exceeded its maximum health check failures, stopping container"
            ),
        );
        docker_client.stop_container(container_ref).await?;
    }

    Ok(())
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

struct ClearHealthCheckLogs {
    seedling_name: seedbank_types::Name,
}

impl ClearHealthCheckLogs {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for ClearHealthCheckLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Clearing seedling '{}' health check logs",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ClearHealthCheckLogs {
    fn name(&self) -> String {
        format!(
            "Clearing seedling '{}' health check logs",
            self.seedling_name
        )
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Clearing seedling '{}' health check logs",
                    self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .reset_health_log(&self.seedling_name)
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
            origin: Some(seedbank_types::Origin::User),
            agent_container_exists: false,
            agent_container_is_running: false,
            agent_container_name: Some(agent_container_name(&name()).unwrap()),
            reached_max_fail_count: false,
            has_health_check_failure: false,
        }
    }

    fn health_check() -> seedbank_types::HealthCheck {
        seedbank_types::HealthCheck {
            command: "true".parse().unwrap(),
            wait_time_in_seconds: std::num::NonZeroU8::new(1).unwrap(),
        }
    }

    fn agent_ip() -> std::net::Ipv4Addr {
        std::net::Ipv4Addr::new(10, 0, 0, 2)
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_start_a_startable_container() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            startable_state(),
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_seedling_is_not_defined() {
        let result = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                seedling_exists: false,
                ..startable_state()
            },
            RequestedBy::Operator,
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
            &health_check(),
            agent_ip(),
            State {
                seedling_credentials_exist: false,
                ..startable_state()
            },
            RequestedBy::Operator,
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
            &health_check(),
            agent_ip(),
            State {
                mounts_initialized: false,
                ..startable_state()
            },
            RequestedBy::Operator,
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
            &health_check(),
            agent_ip(),
            State {
                container_is_startable: false,
                ..startable_state()
            },
            RequestedBy::Operator,
        );

        assert!(matches!(
            result,
            Err(StartSeedlingError::CannotStartSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_recheck_health_when_already_running_with_an_outstanding_failure() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                container_is_startable: false,
                has_health_check_failure: true,
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_to_start_a_core_seedling_for_an_operator() {
        let result = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                origin: Some(seedbank_types::Origin::Core),
                ..startable_state()
            },
            RequestedBy::Operator,
        );

        assert!(matches!(result, Err(StartSeedlingError::CoreSeedling(_))));
    }

    #[test]
    fn test_create_plan_should_allow_the_watchdog_to_start_a_core_seedling() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                origin: Some(seedbank_types::Origin::Core),
                ..startable_state()
            },
            RequestedBy::Watchdog,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_start_a_stopped_agent_container_before_the_app() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                agent_container_exists: true,
                agent_container_is_running: false,
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting agent container 'doug-agent.traefik'",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_agent_when_it_does_not_exist() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                agent_container_exists: false,
                agent_container_is_running: false,
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_agent_when_already_running() {
        let steps = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            State {
                agent_container_exists: true,
                agent_container_is_running: true,
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
                "Setting seedling 'traefik' desired running status to running",
            ]
        );
    }
}
