use crate::{
    blueprints::{
        ContainerPresence, EXPECTED_MOUNT_MODE, IgnoreMissing, RequestedBy, container_name,
        core_seedling_forbidden_for, observe_container, provision_seedling_secrets,
    },
    labels,
    rolodex::{Rolodex, RolodexError, ServiceAccount},
};
use async_trait::async_trait;
use blueprint::{Command, Step, bootstrap::run_plan, push_step};
use bract_types::agent_container_name;
use config::DouglasFolders;
use docker::client::{ContainerRef, ImageRef};
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
    docker_client: &'a dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

#[derive(Debug, PartialEq)]
enum Readiness {
    MountsNotInitialized,
    Stopped,
    Running,
}

#[derive(Debug)]
struct Found {
    container_name: docker_types::ContainerName,
    version: seedbank_types::Version,
    origin: Option<seedbank_types::Origin>,
    has_health_check_failure: bool,
    readiness: Readiness,
}

#[derive(Debug)]
enum Seedling {
    Undefined,
    MissingCredentials,
    GaveUp,
    MissingImage,
    MissingContainer,
    Container(Found),
}

#[derive(Debug)]
struct State {
    agent_container: ContainerPresence,
    agent_container_name: docker_types::ContainerName,
    seedling: Seedling,
}

pub(crate) struct Dependencies<'a> {
    pub inspect: &'a dyn Inspect,
    pub file_reader: &'a dyn FileReader,
    pub permissions: &'a dyn Permissions,
    pub douglas_folders: &'a DouglasFolders,
    pub docker_client: &'a dyn docker::client::Client,
    pub seedbank_client: &'a dyn seedbank_client::Client,
    pub rolodex: &'a dyn Rolodex,
    pub registry: &'a docker_types::Registry,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    deps: Dependencies<'_>,
    name: &seedbank_types::Name,
    requested_by: RequestedBy,
) -> Result<(), StartSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Starting seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver {
            docker_client: deps.docker_client,
            seedbank_client: deps.seedbank_client,
            rolodex: deps.rolodex,
            douglas_folders: deps.douglas_folders,
            inspect: deps.inspect,
            file_reader: deps.file_reader,
            permissions: deps.permissions,
            registry: deps.registry,
            requested_by,
        };
        state_observer.discover(guard.span(), name).await?
    };

    let seedling = deps.seedbank_client.load(name).await?;
    let (_, agent_ip) = provision_seedling_secrets::agent_private_network(&seedling.id);

    let result = {
        let mut context = Context {
            docker_client: deps.docker_client,
            seedbank_client: deps.seedbank_client,
        };
        run_plan(
            guard.span(),
            create_plan(
                name,
                &seedling.definition.health_check,
                agent_ip,
                state,
                requested_by,
            ),
            &mut context,
            StartSeedlingError::FailedBoostrap,
        )
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a dyn docker::client::Client,
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

        let agent_container_name = agent_container_name(name)?;
        let agent_container = observe_container(self.docker_client, &agent_container_name).await?;
        let seedling = self.discover_seedling(name).await?;

        guard.finish(Ok(State {
            agent_container,
            agent_container_name,
            seedling,
        }))
    }

    async fn discover_seedling(
        &self,
        name: &seedbank_types::Name,
    ) -> Result<Seedling, StartSeedlingError> {
        if !self.seedbank_client.exists(name).await? {
            return Ok(Seedling::Undefined);
        }

        let Some(service_account) = self.rolodex.find_service_account(name.as_ref())? else {
            return Ok(Seedling::MissingCredentials);
        };

        let health_check_log = self.seedbank_client.health_check_log(name).await?;
        let has_health_check_failure = health_check_log.is_some();

        let reached_max_fail_count = match self.requested_by {
            RequestedBy::Operator => false,
            RequestedBy::Watchdog => health_check_log
                .map(|log| log.reached_max_fail_count())
                .unwrap_or(false),
        };

        if reached_max_fail_count {
            return Ok(Seedling::GaveUp);
        }

        let seedling = self.seedbank_client.load(name).await?;

        if !self
            .docker_client
            .image_exists(
                self.registry,
                ImageRef::Target(docker_types::PullTarget::from(
                    seedling.definition.image.clone(),
                )),
            )
            .await?
        {
            return Ok(Seedling::MissingImage);
        }

        let container_name: ContainerName = container_name(name)?;
        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(container_name.clone()))
            .await?
        {
            return Ok(Seedling::MissingContainer);
        }

        let container_labels = self
            .docker_client
            .container_labels(ContainerRef::FullName(container_name.clone()))
            .await?;
        let origin = labels::get_origin(&container_labels);

        let readiness = if self
            .mounts_are_initialized(&seedling, &service_account)
            .await?
        {
            self.discover_running(&container_name).await?
        } else {
            Readiness::MountsNotInitialized
        };

        Ok(Seedling::Container(Found {
            container_name,
            version: seedling.version,
            origin,
            has_health_check_failure,
            readiness,
        }))
    }

    async fn discover_running(
        &self,
        container_name: &ContainerName,
    ) -> Result<Readiness, StartSeedlingError> {
        let status = self
            .docker_client
            .container_status(ContainerRef::FullName(container_name.clone()))
            .await?;

        Ok(if status == docker_types::Status::Running {
            Readiness::Running
        } else {
            Readiness::Stopped
        })
    }

    async fn mounts_are_initialized(
        &self,
        seedling: &seedbank_types::Seedling,
        service_account: &ServiceAccount,
    ) -> Result<bool, StartSeedlingError> {
        for (mount_name, mount_definition) in &seedling.definition.mounts {
            let expected = self
                .douglas_folders
                .seedling_mount(seedling.name.as_ref(), mount_name.as_ref());

            if !self.inspect.exists(&expected) {
                return Ok(false);
            }

            let (owning_user, owning_group) =
                self.permissions.get_user_and_group_ownership(&expected)?;

            if service_account.user.system_name != owning_user
                || service_account.group.system_name != owning_group
            {
                return Ok(false);
            }

            if self.permissions.get_mode(&expected)? != EXPECTED_MOUNT_MODE {
                return Ok(false);
            }

            for content in mount_definition.contents() {
                if !self.content_is_in_place(&expected, content)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn content_is_in_place(
        &self,
        mount: &std::path::Path,
        content: &MountContents,
    ) -> Result<bool, StartSeedlingError> {
        match content {
            MountContents::FolderOnly(relative_path) => {
                Ok(self.inspect.exists(&mount.join(relative_path)))
            }
            MountContents::File(mount_file) => {
                let expected = mount.join(&mount_file.file_relative_path);
                if !self.inspect.exists(&expected) {
                    return Ok(false);
                }
                Ok(mount_file.contents == self.file_reader.read_all_bytes(&expected)?)
            }
        }
    }
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    health_check: &seedbank_types::HealthCheck,
    agent_ip: std::net::Ipv4Addr,
    state: State,
    requested_by: RequestedBy,
) -> Result<Vec<Step<Context<'a>>>, StartSeedlingError> {
    let mut steps: Vec<Step<Context>> = Vec::new();

    let found = match state.seedling {
        Seedling::Undefined => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "Seedling not defined".to_string(),
            ));
        }
        Seedling::GaveUp => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "Seedling has failed its health checks".to_string(),
            ));
        }
        Seedling::MissingCredentials => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "Seedling credentials not created yet".to_string(),
            ));
        }
        Seedling::MissingImage | Seedling::MissingContainer => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "Docker instance not initialized".to_string(),
            ));
        }
        Seedling::Container(found) => found,
    };

    if core_seedling_forbidden_for(found.origin, requested_by) {
        return Err(StartSeedlingError::CoreSeedling(name.to_string()));
    }

    let already_running = match found.readiness {
        Readiness::MountsNotInitialized => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "Could not initialize".to_string(),
            ));
        }
        Readiness::Running if !found.has_health_check_failure => {
            return Err(StartSeedlingError::CannotStartSeedling(
                "The seedling is already running".to_string(),
            ));
        }
        Readiness::Running => true,
        Readiness::Stopped => false,
    };

    push_step(&mut steps, SetDesiredRunStatusToRunning::new(name.clone()));

    if state.agent_container.exists() && !state.agent_container.is_running() {
        push_step(
            &mut steps,
            StartAgentContainer::new(state.agent_container_name, agent_ip),
        );
    }

    push_step(
        &mut steps,
        StartSeedling::new(
            name.clone(),
            health_check.clone(),
            found.container_name,
            found.version,
            already_running,
        ),
    );

    push_step(&mut steps, ClearHealthCheckLogs::new(name.clone()));
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

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Stopping agent container '{}'", self.container_name),
                ScopeKind::Step,
            )
            .start_guard();

        if let Err(err) = context
            .docker_client
            .stop_container(ContainerRef::FullName(self.container_name.clone()))
            .await
            .ignore_missing()
        {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(err));
        }

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
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

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.already_running {
            return Ok(());
        }

        let guard = span
            .create_child(
                &format!("Stopping seedling '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        if let Err(err) = context
            .docker_client
            .stop_container(self.container_ref())
            .await
            .ignore_missing()
        {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(err));
        }

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
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
        seedbank_client
            .set_desired_run_status(seedling_name, seedbank_types::DesiredRunStatus::Stopped)
            .await?;
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

    fn found() -> Found {
        Found {
            container_name: container_name(&name()).unwrap(),
            version: seedbank_types::Version(1),
            origin: Some(seedbank_types::Origin::User),
            has_health_check_failure: false,
            readiness: Readiness::Stopped,
        }
    }

    fn state_with(seedling: Seedling) -> State {
        State {
            agent_container: ContainerPresence::Absent,
            agent_container_name: agent_container_name(&name()).unwrap(),
            seedling,
        }
    }

    fn startable_state() -> State {
        state_with(Seedling::Container(found()))
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

    fn step_descriptions(steps: Vec<Step<Context<'_>>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    fn test_span() -> Span {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let reporter: Arc<dyn Reporter> = Arc::new(log::ChannelReporter::new(sender));
        Span::new(reporter, "test", ScopeKind::Task)
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
                "Setting seedling 'traefik' desired running status to running",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_seedling_is_not_defined() {
        let result = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            state_with(Seedling::Undefined),
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
            state_with(Seedling::MissingCredentials),
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
            state_with(Seedling::Container(Found {
                readiness: Readiness::MountsNotInitialized,
                ..found()
            })),
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
            state_with(Seedling::Container(Found {
                readiness: Readiness::Running,
                ..found()
            })),
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
            state_with(Seedling::Container(Found {
                readiness: Readiness::Running,
                has_health_check_failure: true,
                ..found()
            })),
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to running",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_to_start_a_core_seedling_for_an_operator() {
        let result = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            state_with(Seedling::Container(Found {
                origin: Some(seedbank_types::Origin::Core),
                ..found()
            })),
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
            state_with(Seedling::Container(Found {
                origin: Some(seedbank_types::Origin::Core),
                ..found()
            })),
            RequestedBy::Watchdog,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to running",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
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
                agent_container: ContainerPresence::Present(docker_types::Status::Exited),
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to running",
                "Starting agent container 'doug-agent.traefik'",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
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
                agent_container: ContainerPresence::Absent,
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to running",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
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
                agent_container: ContainerPresence::Present(docker_types::Status::Running),
                ..startable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to running",
                "Starting seedling 'traefik' (v1)",
                "Clearing seedling 'traefik' health check logs",
            ]
        );
    }

    fn plan_error(state: State) -> String {
        let Err(err) = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            state,
            RequestedBy::Operator,
        ) else {
            panic!("should refuse");
        };
        err.to_string()
    }

    #[test]
    fn test_create_plan_should_say_why_it_refuses_at_each_stage() {
        assert!(plan_error(state_with(Seedling::Undefined)).contains("not defined"));
        assert!(plan_error(state_with(Seedling::MissingCredentials)).contains("credentials"));
        assert!(plan_error(state_with(Seedling::GaveUp)).contains("failed its health checks"));
        assert!(plan_error(state_with(Seedling::MissingImage)).contains("not initialized"));
        assert!(plan_error(state_with(Seedling::MissingContainer)).contains("not initialized"));
    }

    #[test]
    fn test_create_plan_should_refuse_a_core_seedling_before_looking_at_its_mounts() {
        let result = create_plan(
            &name(),
            &health_check(),
            agent_ip(),
            state_with(Seedling::Container(Found {
                origin: Some(seedbank_types::Origin::Core),
                readiness: Readiness::MountsNotInitialized,
                ..found()
            })),
            RequestedBy::Operator,
        );

        assert!(matches!(result, Err(StartSeedlingError::CoreSeedling(_))));
    }

    fn discovery_seedling() -> seedbank_types::Seedling {
        seedling_with_mounts(std::collections::HashMap::new())
    }

    fn seedling_with_mounts(
        mounts: std::collections::HashMap<seedbank_types::Name, seedbank_types::Mount>,
    ) -> seedbank_types::Seedling {
        seedbank_types::Seedling {
            id: seedbank_types::Id { value: 0 },
            name: name(),
            version: seedbank_types::Version(1),
            definition: seedbank_types::SeedlingDefinition::new(
                docker_types::VersionedImageName::specific("hello-world", "1"),
                mounts,
                seedbank_types::Routing::None,
                health_check(),
            ),
        }
    }

    fn service_account() -> crate::rolodex::ServiceAccount {
        let credential = |id: u32| crate::rolodex::Credential {
            id,
            full_name: "doug.traefik".to_string(),
            system_name: "douglas-traefik".to_string(),
        };
        crate::rolodex::ServiceAccount {
            user: credential(1),
            group: credential(2),
        }
    }

    struct Discovery {
        docker_client: docker::MockClient,
        seedbank_client: seedbank_client::MockClient,
        rolodex: crate::rolodex::MockRolodex,
        inspect: file_system::MockInspect,
        file_reader: file_system::MockFileReader,
        permissions: file_system::MockPermissions,
    }

    impl Discovery {
        fn new() -> Self {
            Self::with_mounts(std::collections::HashMap::new())
        }

        fn with_mounts(
            mounts: std::collections::HashMap<seedbank_types::Name, seedbank_types::Mount>,
        ) -> Self {
            let mut docker_client = docker::MockClient::new();
            docker_client
                .expect_container_exists()
                .returning(|_| Ok(false));
            let mut seedbank_client = seedbank_client::MockClient::new();
            seedbank_client.expect_exists().returning(|_| Ok(true));
            seedbank_client
                .expect_health_check_log()
                .returning(|_| Ok(None));
            seedbank_client
                .expect_load()
                .returning(move |_| Ok(seedling_with_mounts(mounts.clone())));
            let mut rolodex = crate::rolodex::MockRolodex::new();
            rolodex
                .expect_find_service_account()
                .returning(|_| Ok(Some(service_account())));
            Self {
                docker_client,
                seedbank_client,
                rolodex,
                inspect: file_system::MockInspect::new(),
                file_reader: file_system::MockFileReader::new(),
                permissions: file_system::MockPermissions::new(),
            }
        }

        async fn discover(&self, requested_by: RequestedBy) -> Seedling {
            let douglas_folders = DouglasFolders::new();
            let registry: docker_types::Registry = "localhost:7376".parse().unwrap();
            let observer = StateObserver {
                docker_client: &self.docker_client,
                seedbank_client: &self.seedbank_client,
                rolodex: &self.rolodex,
                douglas_folders: &douglas_folders,
                inspect: &self.inspect,
                file_reader: &self.file_reader,
                permissions: &self.permissions,
                registry: &registry,
                requested_by,
            };

            observer
                .discover_seedling(&name())
                .await
                .expect("should discover")
        }
    }

    #[tokio::test]
    async fn test_discover_should_report_a_seedling_that_is_not_defined() {
        let mut discovery = Discovery::new();
        discovery.seedbank_client = seedbank_client::MockClient::new();
        discovery
            .seedbank_client
            .expect_exists()
            .returning(|_| Ok(false));

        let seedling = discovery.discover(RequestedBy::Operator).await;

        assert!(matches!(seedling, Seedling::Undefined));
    }

    #[tokio::test]
    async fn test_discover_should_report_missing_credentials() {
        let mut discovery = Discovery::new();
        discovery.rolodex = crate::rolodex::MockRolodex::new();
        discovery
            .rolodex
            .expect_find_service_account()
            .returning(|_| Ok(None));

        let seedling = discovery.discover(RequestedBy::Operator).await;

        assert!(matches!(seedling, Seedling::MissingCredentials));
    }

    fn exhausted_health_check_log() -> seedbank_types::HealthCheckLog {
        seedbank_types::HealthCheckLog {
            fail_count: 6,
            updated_at: std::time::SystemTime::now(),
        }
    }

    #[tokio::test]
    async fn test_discover_should_report_that_the_watchdog_gave_up_after_too_many_failures() {
        let mut discovery = Discovery::new();
        discovery.seedbank_client = seedbank_client::MockClient::new();
        discovery
            .seedbank_client
            .expect_exists()
            .returning(|_| Ok(true));
        discovery
            .seedbank_client
            .expect_health_check_log()
            .returning(|_| Ok(Some(exhausted_health_check_log())));

        let seedling = discovery.discover(RequestedBy::Watchdog).await;

        assert!(matches!(seedling, Seedling::GaveUp));
    }

    #[tokio::test]
    async fn test_discover_should_not_give_up_for_an_operator_however_many_times_it_failed() {
        let mut discovery = Discovery::new();
        discovery.seedbank_client = seedbank_client::MockClient::new();
        discovery
            .seedbank_client
            .expect_exists()
            .returning(|_| Ok(true));
        discovery
            .seedbank_client
            .expect_health_check_log()
            .returning(|_| Ok(Some(exhausted_health_check_log())));
        discovery
            .seedbank_client
            .expect_load()
            .returning(|_| Ok(discovery_seedling()));
        discovery
            .docker_client
            .expect_image_exists()
            .returning(|_, _| Ok(false));

        let seedling = discovery.discover(RequestedBy::Operator).await;

        assert!(matches!(seedling, Seedling::MissingImage));
    }

    #[tokio::test]
    async fn test_discover_should_report_a_missing_container_when_the_image_exists() {
        let mut discovery = Discovery::new();
        discovery
            .docker_client
            .expect_image_exists()
            .returning(|_, _| Ok(true));

        let seedling = discovery.discover(RequestedBy::Operator).await;

        assert!(matches!(seedling, Seedling::MissingContainer));
    }

    async fn discover_container(status: docker_types::Status, failing: bool) -> Found {
        let mut discovery = Discovery::new();
        discovery.docker_client = docker::MockClient::new();
        discovery
            .docker_client
            .expect_image_exists()
            .returning(|_, _| Ok(true));
        discovery
            .docker_client
            .expect_container_exists()
            .returning(|_| Ok(true));
        discovery
            .docker_client
            .expect_container_labels()
            .returning(|_| {
                Ok(vec![labels::create_origin_label(
                    seedbank_types::Origin::Core,
                )])
            });
        discovery
            .docker_client
            .expect_container_status()
            .returning(move |_| Ok(status.clone()));
        if failing {
            discovery.seedbank_client = seedbank_client::MockClient::new();
            discovery
                .seedbank_client
                .expect_exists()
                .returning(|_| Ok(true));
            discovery
                .seedbank_client
                .expect_health_check_log()
                .returning(|_| Ok(Some(seedbank_types::HealthCheckLog::default())));
            discovery
                .seedbank_client
                .expect_load()
                .returning(|_| Ok(discovery_seedling()));
        }

        match discovery.discover(RequestedBy::Operator).await {
            Seedling::Container(found) => found,
            other => panic!("should find the container, got {other:?}"),
        }
    }

    fn a_mount(
        contents: std::collections::HashSet<seedbank_types::MountContents>,
    ) -> std::collections::HashMap<seedbank_types::Name, seedbank_types::Mount> {
        std::collections::HashMap::from([(
            "data".parse().unwrap(),
            seedbank_types::Mount::with_files(
                seedbank_types::MountType::Persisted,
                std::path::PathBuf::from("/data"),
                seedbank_types::AccessMode::Writable,
                contents,
            ),
        )])
    }

    fn relative(path: &str) -> file_system::RelativePath {
        file_system::RelativePath::try_from(std::path::PathBuf::from(path)).unwrap()
    }

    async fn readiness_with_mounts(mut discovery: Discovery) -> Readiness {
        discovery.docker_client = docker::MockClient::new();
        discovery
            .docker_client
            .expect_image_exists()
            .returning(|_, _| Ok(true));
        discovery
            .docker_client
            .expect_container_exists()
            .returning(|_| Ok(true));
        discovery
            .docker_client
            .expect_container_labels()
            .returning(|_| Ok(Vec::new()));
        discovery
            .docker_client
            .expect_container_status()
            .returning(|_| Ok(docker_types::Status::Exited));

        match discovery.discover(RequestedBy::Operator).await {
            Seedling::Container(found) => found.readiness,
            other => panic!("should find the container, got {other:?}"),
        }
    }

    fn mount_is_in_place(discovery: &mut Discovery) {
        discovery.inspect.expect_exists().returning(|_| true);
        discovery
            .permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Ok(("douglas-traefik".to_string(), "douglas-traefik".to_string())));
        discovery
            .permissions
            .expect_get_mode()
            .returning(|_| Ok(EXPECTED_MOUNT_MODE));
    }

    #[tokio::test]
    async fn test_discover_should_report_ready_when_every_mount_is_in_place() {
        let mut discovery = Discovery::with_mounts(a_mount(std::collections::HashSet::new()));
        mount_is_in_place(&mut discovery);

        assert_eq!(readiness_with_mounts(discovery).await, Readiness::Stopped);
    }

    #[tokio::test]
    async fn test_discover_should_report_mounts_not_initialized_when_the_mount_folder_is_missing() {
        let mut discovery = Discovery::with_mounts(a_mount(std::collections::HashSet::new()));
        discovery.inspect.expect_exists().returning(|_| false);

        assert_eq!(
            readiness_with_mounts(discovery).await,
            Readiness::MountsNotInitialized
        );
    }

    #[tokio::test]
    async fn test_discover_should_report_mounts_not_initialized_when_the_owner_is_wrong() {
        let mut discovery = Discovery::with_mounts(a_mount(std::collections::HashSet::new()));
        discovery.inspect.expect_exists().returning(|_| true);
        discovery
            .permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Ok(("root".to_string(), "root".to_string())));

        assert_eq!(
            readiness_with_mounts(discovery).await,
            Readiness::MountsNotInitialized
        );
    }

    #[tokio::test]
    async fn test_discover_should_report_mounts_not_initialized_when_the_mode_is_wrong() {
        let mut discovery = Discovery::with_mounts(a_mount(std::collections::HashSet::new()));
        discovery.inspect.expect_exists().returning(|_| true);
        discovery
            .permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Ok(("douglas-traefik".to_string(), "douglas-traefik".to_string())));
        discovery
            .permissions
            .expect_get_mode()
            .returning(|_| Ok(file_system::Modes::OwnerReadWrite));

        assert_eq!(
            readiness_with_mounts(discovery).await,
            Readiness::MountsNotInitialized
        );
    }

    #[tokio::test]
    async fn test_discover_should_report_mounts_not_initialized_when_a_folder_inside_is_missing() {
        let contents =
            std::collections::HashSet::from([seedbank_types::MountContents::FolderOnly(relative(
                "logs",
            ))]);
        let mut discovery = Discovery::with_mounts(a_mount(contents));
        discovery
            .inspect
            .expect_exists()
            .returning(|path| !path.ends_with("logs"));
        discovery
            .permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Ok(("douglas-traefik".to_string(), "douglas-traefik".to_string())));
        discovery
            .permissions
            .expect_get_mode()
            .returning(|_| Ok(EXPECTED_MOUNT_MODE));

        assert_eq!(
            readiness_with_mounts(discovery).await,
            Readiness::MountsNotInitialized
        );
    }

    fn mount_with_file(
        contents: &[u8],
    ) -> std::collections::HashMap<seedbank_types::Name, seedbank_types::Mount> {
        a_mount(std::collections::HashSet::from([
            seedbank_types::MountContents::File(seedbank_types::MountFile::new(
                relative("config.yml"),
                contents.to_vec(),
            )),
        ]))
    }

    #[tokio::test]
    async fn test_discover_should_report_ready_when_a_mounted_file_has_the_expected_contents() {
        let mut discovery = Discovery::with_mounts(mount_with_file(b"expected"));
        mount_is_in_place(&mut discovery);
        discovery
            .file_reader
            .expect_read_all_bytes()
            .returning(|_| Ok(b"expected".to_vec()));

        assert_eq!(readiness_with_mounts(discovery).await, Readiness::Stopped);
    }

    #[tokio::test]
    async fn test_discover_should_report_mounts_not_initialized_when_a_mounted_file_has_other_contents()
     {
        let mut discovery = Discovery::with_mounts(mount_with_file(b"expected"));
        mount_is_in_place(&mut discovery);
        discovery
            .file_reader
            .expect_read_all_bytes()
            .returning(|_| Ok(b"something else".to_vec()));

        assert_eq!(
            readiness_with_mounts(discovery).await,
            Readiness::MountsNotInitialized
        );
    }

    #[tokio::test]
    async fn test_discover_should_report_a_stopped_container_with_its_version_and_origin() {
        let found = discover_container(docker_types::Status::Exited, false).await;

        assert_eq!(found.readiness, Readiness::Stopped);
        assert_eq!(found.version, seedbank_types::Version(1));
        assert_eq!(found.origin, Some(seedbank_types::Origin::Core));
        assert!(!found.has_health_check_failure);
    }

    #[tokio::test]
    async fn test_discover_should_report_a_running_container() {
        let found = discover_container(docker_types::Status::Running, false).await;

        assert_eq!(found.readiness, Readiness::Running);
    }

    #[tokio::test]
    async fn test_discover_should_carry_an_outstanding_health_check_failure() {
        let found = discover_container(docker_types::Status::Running, true).await;

        assert!(found.has_health_check_failure);
    }

    #[tokio::test]
    async fn test_record_health_check_failure_should_leave_desired_status_alone_below_the_threshold()
     {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_stop_container().times(0);

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_increment_health_log_fail_count()
            .returning(|_| Ok(false));
        seedbank_client.expect_set_desired_run_status().times(0);

        let span = test_span();
        record_health_check_failure(
            &span,
            &docker_client,
            &seedbank_client,
            &name(),
            ContainerRef::FullName(container_name(&name()).unwrap()),
        )
        .await
        .expect("should record the failure");
    }

    #[tokio::test]
    async fn test_record_health_check_failure_should_stop_the_container_and_mark_desired_status_stopped_at_the_threshold()
     {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_stop_container().returning(|_| Ok(()));

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_increment_health_log_fail_count()
            .returning(|_| Ok(true));
        seedbank_client
            .expect_set_desired_run_status()
            .withf(|_, status| *status == seedbank_types::DesiredRunStatus::Stopped)
            .returning(|_, _| Ok(()));

        let span = test_span();
        record_health_check_failure(
            &span,
            &docker_client,
            &seedbank_client,
            &name(),
            ContainerRef::FullName(container_name(&name()).unwrap()),
        )
        .await
        .expect("should record the failure");
    }

    #[tokio::test]
    async fn test_start_agent_container_rollback_should_stop_the_agent_container() {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_stop_container().returning(|_| Ok(()));
        let seedbank_client = seedbank_client::MockClient::new();
        let mut context = Context {
            docker_client: &docker_client,
            seedbank_client: &seedbank_client,
        };

        let mut command =
            StartAgentContainer::new(agent_container_name(&name()).unwrap(), agent_ip());
        let span = test_span();

        let result = command.rollback(&span, &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_start_seedling_rollback_should_stop_the_container_when_it_was_not_already_running()
     {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_stop_container().returning(|_| Ok(()));
        let seedbank_client = seedbank_client::MockClient::new();
        let mut context = Context {
            docker_client: &docker_client,
            seedbank_client: &seedbank_client,
        };

        let mut command = StartSeedling::new(
            name(),
            health_check(),
            container_name(&name()).unwrap(),
            seedbank_types::Version(1),
            false,
        );
        let span = test_span();

        let result = command.rollback(&span, &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_start_seedling_rollback_should_leave_an_already_running_container_alone() {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_stop_container().never();
        let seedbank_client = seedbank_client::MockClient::new();
        let mut context = Context {
            docker_client: &docker_client,
            seedbank_client: &seedbank_client,
        };

        let mut command = StartSeedling::new(
            name(),
            health_check(),
            container_name(&name()).unwrap(),
            seedbank_types::Version(1),
            true,
        );
        let span = test_span();

        let result = command.rollback(&span, &mut context).await;

        assert!(result.is_ok());
    }
}
