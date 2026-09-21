use crate::{
    blueprints::{RequestedBy, container_name, provision_seedling_secrets},
    rolodex::Rolodex,
};
use async_trait::async_trait;
use blueprint::{Command, Step, bootstrap::run_plan, push_step};
use config::DouglasFolders;
use credentials::Credentials;
use docker::client::ContainerRef;
use docker_types::{ContainerName, DockerNameError};
use file_system::{
    FileDeleter, FileReader, FileWriter, Folder, FolderDeleter, Inspect, Permissions,
};
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
    file_deleter: &'a dyn FileDeleter,
    folder_deleter: &'a dyn FolderDeleter,
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

pub(crate) struct Dependencies<'a> {
    pub docker_client: &'a dyn docker::client::Client,
    pub seedbank_client: &'a dyn seedbank_client::Client,
    pub credentials: &'a dyn Credentials,
    pub inspect: &'a dyn Inspect,
    pub folder: &'a dyn Folder,
    pub file_reader: &'a dyn FileReader,
    pub file_writer: &'a dyn FileWriter,
    pub file_deleter: &'a dyn FileDeleter,
    pub folder_deleter: &'a dyn FolderDeleter,
    pub permissions: &'a dyn Permissions,
    pub douglas_folders: &'a DouglasFolders,
    pub resin_client_builder: &'a dyn resin_client::ClientBuilder,
    pub registry: &'a docker_types::Registry,
    pub rolodex: &'a dyn Rolodex,
    pub agent_provisioning: Option<&'a provision_seedling_secrets::AgentProvisioning>,
    pub ram_disk: &'a dyn RamDisk,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    deps: Dependencies<'_>,
) -> Result<(), WatchdogError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Running watchdog sweep",
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver {
            docker_client: deps.docker_client,
            seedbank_client: deps.seedbank_client,
        };
        state_observer.discover(guard.span()).await?
    };

    let result = {
        let mut context = Context {
            docker_client: deps.docker_client,
            seedbank_client: deps.seedbank_client,
            credentials: deps.credentials,
            inspect: deps.inspect,
            folder: deps.folder,
            file_reader: deps.file_reader,
            file_writer: deps.file_writer,
            file_deleter: deps.file_deleter,
            folder_deleter: deps.folder_deleter,
            permissions: deps.permissions,
            douglas_folders: deps.douglas_folders,
            resin_client_builder: deps.resin_client_builder,
            registry: deps.registry,
            rolodex: deps.rolodex,
            agent_provisioning: deps.agent_provisioning,
            ram_disk: deps.ram_disk,
        };
        run_plan(
            guard.span(),
            create_plan(state),
            &mut context,
            WatchdogError::FailedBoostrap,
        )
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

impl<'a> StateObserver<'a> {
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
            crate::blueprints::reconcile_seedling::Dependencies {
                credentials: &*context.credentials,
                inspect: &*context.inspect,
                folder: &*context.folder,
                file_reader: &*context.file_reader,
                file_writer: &*context.file_writer,
                file_deleter: &*context.file_deleter,
                folder_deleter: &*context.folder_deleter,
                permissions: &*context.permissions,
                douglas_folders: &*context.douglas_folders,
                docker_client: context.docker_client,
                resin_client_builder: &*context.resin_client_builder,
                seedbank_client: &*context.seedbank_client,
                registry: &*context.registry,
                rolodex: &*context.rolodex,
                ram_disk: &*context.ram_disk,
            },
            &self.seedling_name,
            &seedling.version,
            &seedling.definition,
            context.agent_provisioning,
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
            crate::blueprints::start_seedling::Dependencies {
                inspect: &*context.inspect,
                file_reader: &*context.file_reader,
                permissions: &*context.permissions,
                douglas_folders: &*context.douglas_folders,
                docker_client: context.docker_client,
                seedbank_client: &*context.seedbank_client,
                rolodex: &*context.rolodex,
                registry: &*context.registry,
            },
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
