use crate::{
    blueprints::{RequestedBy, container_name, provision_seedling_secrets},
    rolodex::Rolodex,
};
use async_trait::async_trait;
use blueprint::{
    Command, Step,
    bootstrap::{execute_plan, resolve_plan},
    push_step,
};
use config::DouglasFolders;
use credentials::Credentials;
use docker::client::ContainerRef;
use docker_types::{ContainerName, DockerNameError};
use file_system::{FileReader, FileWriter, Folder, Inspect, Permissions};
use log::{Reporter, ScopeKind, Span};
use ram_disk::RamDisk;
use seedbank_types::DesiredRunStatus;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WatchdogError {
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Seedbank error {0}")]
    SeedbankError(#[from] seedbank_client::Error),
}

struct Context<'a> {
    docker_client: &'a dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    credentials: &'a dyn Credentials,
    inspect: &'a dyn Inspect,
    folder: &'a dyn Folder,
    file_reader: &'a dyn FileReader,
    file_writer: &'a dyn FileWriter,
    permissions: &'a dyn Permissions,
    douglas_folders: &'a DouglasFolders,
    resin_client_builder: &'a dyn resin_client::ClientBuilder,
    registry: &'a docker_types::Registry,
    rolodex: &'a dyn Rolodex,
    agent_provisioning: Option<&'a provision_seedling_secrets::AgentProvisioning>,
    ram_disk: &'a dyn RamDisk,
}

#[derive(Debug, Default)]
struct State {
    needing_start: Vec<seedbank_types::Name>,
    needing_stop: Vec<seedbank_types::Name>,
    needing_health_recheck: Vec<seedbank_types::Name>,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client: &dyn docker::client::Client,
    seedbank_client: &dyn seedbank_client::Client,
    credentials: &dyn Credentials,
    inspect: &dyn Inspect,
    folder: &dyn Folder,
    file_reader: &dyn FileReader,
    file_writer: &dyn FileWriter,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
    resin_client_builder: &dyn resin_client::ClientBuilder,
    registry: &docker_types::Registry,
    rolodex: &dyn Rolodex,
    agent_provisioning: Option<&provision_seedling_secrets::AgentProvisioning>,
    ram_disk: &dyn RamDisk,
) -> Result<(), WatchdogError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Running watchdog sweep",
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver::new(docker_client, seedbank_client);
        state_observer.discover(guard.span()).await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            docker_client,
            seedbank_client,
            credentials,
            inspect,
            folder,
            file_reader,
            file_writer,
            permissions,
            douglas_folders,
            resin_client_builder,
            registry,
            rolodex,
            agent_provisioning,
            ram_disk,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            WatchdogError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a dyn docker::client::Client,
        seedbank_client: &'a dyn seedbank_client::Client,
    ) -> Self {
        Self {
            docker_client,
            seedbank_client,
        }
    }

    pub async fn discover(&mut self, span: &Span) -> Result<State, WatchdogError> {
        let guard = span
            .create_child(
                "Watchdog sweep, discovering seedling state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State::default();

        let seedling_names = self.seedbank_client.list().await?;
        for name in seedling_names.iter() {
            let desired_status = self.seedbank_client.get_desired_run_status(name).await?;
            let container_is_running = self.container_is_running(name).await?;

            let reached_max_fail_count = match self.seedbank_client.health_check_log(name).await? {
                Some(health_check_log) => health_check_log.reached_max_fail_count(),
                None => false,
            };

            if container_is_running {
                if reached_max_fail_count || desired_status == DesiredRunStatus::Stopped {
                    guard.span().message(
                        log::Level::Info,
                        &format!(
                            "'{name}' is running but should be stopped (desired={desired_status:?}, reached_max_fail_count={reached_max_fail_count})"
                        ),
                    );
                    result.needing_stop.push(name.clone());
                } else {
                    guard.span().message(
                        log::Level::Info,
                        &format!("'{name}' is running, rechecking its health"),
                    );
                    result.needing_health_recheck.push(name.clone());
                }
            } else if !reached_max_fail_count && desired_status == DesiredRunStatus::Running {
                guard.span().message(
                    log::Level::Info,
                    &format!("'{name}' is not running but desired status is Running"),
                );
                result.needing_start.push(name.clone());
            } else if reached_max_fail_count {
                guard.span().message(
                    log::Level::Warn,
                    &format!(
                        "'{name}' is not running and has exceeded its maximum health check failures — not retrying automatically"
                    ),
                );
            }
        }

        guard.span().message(
            log::Level::Info,
            &format!(
                "Watchdog sweep found {} seedling(s) needing stop, {} needing start, {} needing a health recheck",
                result.needing_stop.len(),
                result.needing_start.len(),
                result.needing_health_recheck.len()
            ),
        );

        guard.finish(Ok(result))
    }

    async fn container_is_running(
        &self,
        name: &seedbank_types::Name,
    ) -> Result<bool, WatchdogError> {
        let container_name: ContainerName = container_name(name)?;

        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(container_name.clone()))
            .await?
        {
            return Ok(false);
        }

        Ok(matches!(
            self.docker_client
                .container_status(ContainerRef::FullName(container_name))
                .await?,
            docker_types::Status::Running
        ))
    }
}

fn create_plan<'a>(state: State) -> Result<Vec<Step<Context<'a>>>, WatchdogError> {
    let mut steps: Vec<Step<Context>> = Vec::new();

    for seedling_name in state.needing_stop.iter() {
        push_step(&mut steps, StopSeedling::new(seedling_name.clone()));
    }

    for seedling_name in state.needing_start.iter() {
        push_step(&mut steps, ReconcileSeedling::new(seedling_name.clone()));
        push_step(&mut steps, StartSeedling::new(seedling_name.clone()));
    }

    for seedling_name in state.needing_health_recheck.iter() {
        push_step(
            &mut steps,
            RecheckSeedlingHealth::new(seedling_name.clone()),
        );
    }

    Ok(steps)
}

struct StopSeedling {
    seedling_name: seedbank_types::Name,
}

impl StopSeedling {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for StopSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stop seedling '{}' ", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StopSeedling {
    fn name(&self) -> String {
        "Stop seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Stopping seedling '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        crate::blueprints::stop_seedling::execute(
            Arc::clone(&span.reporter),
            context.docker_client,
            context.seedbank_client,
            &self.seedling_name,
            RequestedBy::Watchdog,
        )
        .await?;

        guard.finish(Ok(()))
    }
}

struct ReconcileSeedling {
    seedling_name: seedbank_types::Name,
}

impl ReconcileSeedling {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for ReconcileSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Reconcile seedling '{}' ", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ReconcileSeedling {
    fn name(&self) -> String {
        "Reconcile seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Reconciling seedling '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let seedling = context.seedbank_client.load(&self.seedling_name).await?;

        crate::blueprints::reconcile_seedling::execute(
            Arc::clone(&span.reporter),
            &*context.credentials,
            &*context.inspect,
            &*context.folder,
            &*context.file_reader,
            &*context.file_writer,
            &*context.permissions,
            &*context.douglas_folders,
            context.docker_client,
            &*context.resin_client_builder,
            &*context.seedbank_client,
            &*context.registry,
            &*context.rolodex,
            &self.seedling_name,
            &seedling.version,
            &seedling.definition,
            context.agent_provisioning,
            &*context.ram_disk,
        )
        .await?;

        guard.finish(Ok(()))
    }
}

struct StartSeedling {
    seedling_name: seedbank_types::Name,
}

impl StartSeedling {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for StartSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Start seedling '{}' ", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartSeedling {
    fn name(&self) -> String {
        "Start seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Starting seedling '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        crate::blueprints::start_seedling::execute(
            Arc::clone(&span.reporter),
            &*context.inspect,
            &*context.file_reader,
            &*context.permissions,
            &*context.douglas_folders,
            context.docker_client,
            &*context.seedbank_client,
            &*context.rolodex,
            &*context.registry,
            &self.seedling_name,
            RequestedBy::Watchdog,
        )
        .await?;

        guard.finish(Ok(()))
    }
}

struct RecheckSeedlingHealth {
    seedling_name: seedbank_types::Name,
}

impl RecheckSeedlingHealth {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for RecheckSeedlingHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rechecking health for seedling '{}' ",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RecheckSeedlingHealth {
    fn name(&self) -> String {
        "Rechecking seedling health".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Rechecking health for seedling '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let seedling = context.seedbank_client.load(&self.seedling_name).await?;
        let container = container_name(&self.seedling_name)?;

        let is_healthy = crate::blueprints::start_seedling::run_shell_health_check(
            guard.span(),
            context.docker_client,
            ContainerRef::FullName(container.clone()),
            &seedling.definition.health_check.command.to_string(),
        )
        .await?;

        if is_healthy {
            context
                .seedbank_client
                .reset_health_log(&self.seedling_name)
                .await?;
            return guard.finish(Ok(()));
        }

        crate::blueprints::start_seedling::record_health_check_failure(
            guard.span(),
            context.docker_client,
            context.seedbank_client,
            &self.seedling_name,
            ContainerRef::FullName(container),
        )
        .await?;

        guard.finish(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "always-fails".parse().unwrap()
    }

    fn step_descriptions(steps: Vec<Step<Context<'_>>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_recheck_health_for_a_running_seedling_below_the_fail_threshold() {
        let steps = create_plan(State {
            needing_health_recheck: vec![name()],
            ..State::default()
        })
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Rechecking health for seedling 'always-fails' "]
        );
    }

    #[test]
    fn test_create_plan_should_stop_before_starting_before_rechecking() {
        let steps = create_plan(State {
            needing_stop: vec![name()],
            needing_start: vec![name()],
            needing_health_recheck: vec![name()],
        })
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Stop seedling 'always-fails' ",
                "Reconcile seedling 'always-fails' ",
                "Start seedling 'always-fails' ",
                "Rechecking health for seedling 'always-fails' ",
            ]
        );
    }
}
