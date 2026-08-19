use crate::blueprints::{container_name, seedling_network_name, traefik_dynamic_dir};
use crate::labels;
use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use config::DouglasFolders;
use docker::client::{ClientBuilder, ContainerRef};
use docker_types::DockerNameError;
use file_system::{FileDeleter, FileReader, FileSystemError};
use log::{Reporter, ScopeKind, Span};
use std::sync::Arc;
use thiserror::Error;

const TRAEFIK_SEEDLING_NAME: &str = "traefik";

#[derive(Error, Debug)]
pub enum DropSeedlingError {
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Cannot drop seedling {0}")]
    CannotDropSeedling(String),
    #[error("Cannot drop seedling {0}: it is a core seedling managed by douglas")]
    CoreSeedling(String),
    #[error("Seedbank error: {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Resin error: {0}")]
    ResinError(#[from] resin_client::Error),
    #[error("Name parse error: {0}")]
    NameParseError(#[from] seedbank_types::NameParseError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

struct Context<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    resin_client: &'a mut dyn resin_client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    file_deleter: &'a dyn FileDeleter,
    douglas_folders: &'a DouglasFolders,
}

#[derive(Debug)]
struct State {
    container_exists: bool,
    container_is_stopped: bool,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
    origin: Option<labels::Origin>,
    network_exists: bool,
    seedbank_entry_exists: bool,
    route_exists: bool,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client_builder: &dyn ClientBuilder,
    resin_client_builder: &dyn resin_client::ClientBuilder,
    seedbank_client: &dyn seedbank_client::Client,
    file_deleter: &dyn FileDeleter,
    file_reader: &dyn FileReader,
    douglas_folders: &DouglasFolders,
    name: &seedbank_types::Name,
) -> Result<(), DropSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Dropping seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut docker_client = match docker_client_builder.build(Arc::clone(&reporter)).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(DropSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let mut resin_client = match resin_client_builder.build(Arc::clone(&reporter)).await {
        Ok(resin_client) => resin_client,
        Err(err) => {
            return guard.finish(Err(DropSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let state = {
        let mut state_observer =
            StateObserver::new(&mut *docker_client, seedbank_client, file_reader);
        state_observer
            .discover(guard.span(), douglas_folders, name)
            .await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(name, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            docker_client: &mut *docker_client,
            resin_client: &mut *resin_client,
            seedbank_client,
            file_deleter,
            douglas_folders,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            DropSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    file_reader: &'a dyn FileReader,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a mut dyn docker::client::Client,
        seedbank_client: &'a dyn seedbank_client::Client,
        file_reader: &'a dyn FileReader,
    ) -> Self {
        Self {
            docker_client,
            seedbank_client,
            file_reader,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
        name: &seedbank_types::Name,
    ) -> Result<State, DropSeedlingError> {
        let guard = span
            .create_child(
                "Dropping seedling, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut route_path = traefik_dynamic_dir(douglas_folders)?;
        route_path.push(format!("{name}.yml"));

        let mut result = State {
            container_exists: false,
            container_is_stopped: false,
            container_name: container_name(name)?,
            version: None,
            origin: None,
            network_exists: false,
            seedbank_entry_exists: self.seedbank_client.exists(name).await?,
            route_exists: self.file_reader.exists(&route_path),
        };

        result.network_exists = self
            .docker_client
            .network_exists(&seedling_network_name(name)?)
            .await?;

        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(result.container_name.clone()))
            .await?
        {
            return guard.finish(Ok(result));
        }
        result.container_exists = true;
        result.container_is_stopped = matches!(
            self.docker_client
                .container_status(ContainerRef::FullName(result.container_name.clone()))
                .await?,
            docker_types::Status::Created
                | docker_types::Status::Exited
                | docker_types::Status::Dead
        );

        let container_labels = self
            .docker_client
            .container_labels(ContainerRef::FullName(result.container_name.clone()))
            .await?;
        result.origin = labels::get_origin(&container_labels);
        result.version = labels::get_version(&container_labels).ok();

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
) -> Result<Vec<Step<'a>>, DropSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

    if state.origin == Some(labels::Origin::Core) {
        return Err(DropSeedlingError::CoreSeedling(name.to_string()));
    }
    if state.container_exists && !state.container_is_stopped {
        return Err(DropSeedlingError::CannotDropSeedling(
            "Seedling is not stopped".to_string(),
        ));
    }

    if state.route_exists {
        push_step(&mut steps, RemoveTraefikRoute::new(name.clone()));
    }

    if state.container_exists {
        push_step(
            &mut steps,
            DropSeedling::new(name.clone(), state.container_name, state.version),
        );
    }

    if state.network_exists {
        push_step(&mut steps, DeleteSeedlingNetwork::new(name.clone()));
    }

    if state.seedbank_entry_exists {
        push_step(&mut steps, DeleteSeedbankEntry::new(name.clone()));
    }

    push_step(&mut steps, ReleaseDefault::new(name.clone()));
    push_step(&mut steps, DeleteResinRepository::new(name.clone()));

    Ok(steps)
}

struct DropSeedling {
    seedling_name: seedbank_types::Name,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
}

impl DropSeedling {
    pub fn new(
        seedling_name: seedbank_types::Name,
        container_name: docker_types::ContainerName,
        version: Option<seedbank_types::Version>,
    ) -> Self {
        Self {
            seedling_name,
            container_name,
            version,
        }
    }
}

impl std::fmt::Display for DropSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.version {
            Some(version) => write!(f, "Dropping seedling '{}' (v{version})", self.seedling_name),
            None => write!(f, "Dropping seedling '{}'", self.seedling_name),
        }
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DropSeedling {
    fn name(&self) -> String {
        "Dropping seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let message = match &self.version {
            Some(version) => format!("Dropping seedling '{}' (v{version})…", self.seedling_name),
            None => format!("Dropping seedling '{}'…", self.seedling_name),
        };
        let guard = span.create_child(&message, ScopeKind::Step).start_guard();

        context
            .docker_client
            .delete_container(ContainerRef::FullName(self.container_name.clone()))
            .await?;

        guard.finish(Ok(()))
    }
}

struct RemoveTraefikRoute {
    seedling_name: seedbank_types::Name,
}

impl RemoveTraefikRoute {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for RemoveTraefikRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Removing traefik route for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RemoveTraefikRoute {
    fn name(&self) -> String {
        "Removing traefik route".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Removing traefik route for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let traefik_name: seedbank_types::Name = TRAEFIK_SEEDLING_NAME.parse()?;
        let traefik_container = container_name(&traefik_name)?;
        let seedling_network = seedling_network_name(&self.seedling_name)?;

        context
            .docker_client
            .disconnect_network(&seedling_network, ContainerRef::FullName(traefik_container))
            .await?;

        let mut route_path = traefik_dynamic_dir(context.douglas_folders)?;
        route_path.push(format!("{}.yml", self.seedling_name));
        context.file_deleter.delete(&route_path)?;

        guard.finish(Ok(()))
    }
}

struct DeleteSeedlingNetwork {
    seedling_name: seedbank_types::Name,
}

impl DeleteSeedlingNetwork {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for DeleteSeedlingNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Deleting network for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DeleteSeedlingNetwork {
    fn name(&self) -> String {
        "Deleting network".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Deleting network for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let network = seedling_network_name(&self.seedling_name)?;
        context.docker_client.delete_network(&network).await?;

        guard.finish(Ok(()))
    }
}

struct DeleteSeedbankEntry {
    seedling_name: seedbank_types::Name,
}

impl DeleteSeedbankEntry {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for DeleteSeedbankEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Deleting seedbank entry for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DeleteSeedbankEntry {
    fn name(&self) -> String {
        "Deleting seedbank entry".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Deleting seedbank entry for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        context.seedbank_client.delete(&self.seedling_name).await?;

        guard.finish(Ok(()))
    }
}

struct ReleaseDefault {
    seedling_name: seedbank_types::Name,
}

impl ReleaseDefault {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for ReleaseDefault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Releasing default for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ReleaseDefault {
    fn name(&self) -> String {
        "Releasing default".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Releasing default for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .release_default(&self.seedling_name)
            .await?;

        guard.finish(Ok(()))
    }
}

struct DeleteResinRepository {
    seedling_name: seedbank_types::Name,
}

impl DeleteResinRepository {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for DeleteResinRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Deleting resin repository for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DeleteResinRepository {
    fn name(&self) -> String {
        "Deleting resin repository".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Deleting resin repository for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let resin_name: resin_types::Name = self.seedling_name.as_ref().parse()?;
        context.resin_client.delete_repository(&resin_name).await?;

        guard.finish(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "hello-world".parse().unwrap()
    }

    fn droppable_state() -> State {
        State {
            container_exists: true,
            container_is_stopped: true,
            container_name: container_name(&name()).unwrap(),
            version: Some(seedbank_types::Version(1)),
            origin: Some(labels::Origin::User),
            network_exists: true,
            seedbank_entry_exists: true,
            route_exists: true,
        }
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_fully_tear_down_a_stopped_seedling() {
        let steps = create_plan(&name(), droppable_state()).expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Removing traefik route for 'hello-world'".to_string(),
                "Dropping seedling 'hello-world' (v1)".to_string(),
                "Deleting network for 'hello-world'".to_string(),
                "Deleting seedbank entry for 'hello-world'".to_string(),
                "Releasing default for 'hello-world'".to_string(),
                "Deleting resin repository for 'hello-world'".to_string(),
            ]
        );
    }

    #[test]
    fn test_create_plan_should_omit_the_version_when_it_could_not_be_determined() {
        let steps = create_plan(
            &name(),
            State {
                version: None,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert!(step_descriptions(steps).contains(&"Dropping seedling 'hello-world'".to_string()));
    }

    #[test]
    fn test_create_plan_should_skip_container_and_network_steps_when_absent_but_still_clean_up_the_rest()
     {
        let steps = create_plan(
            &name(),
            State {
                container_exists: false,
                container_is_stopped: false,
                network_exists: false,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Removing traefik route for 'hello-world'".to_string(),
                "Deleting seedbank entry for 'hello-world'".to_string(),
                "Releasing default for 'hello-world'".to_string(),
                "Deleting resin repository for 'hello-world'".to_string(),
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_traefik_route_step_when_no_route_was_ever_written() {
        let steps = create_plan(
            &name(),
            State {
                route_exists: false,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert!(
            !step_descriptions(steps)
                .iter()
                .any(|description| description.contains("traefik route"))
        );
    }

    #[test]
    fn test_create_plan_should_skip_the_seedbank_step_when_no_entry_exists() {
        let steps = create_plan(
            &name(),
            State {
                seedbank_entry_exists: false,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert!(
            !step_descriptions(steps)
                .iter()
                .any(|description| description.contains("seedbank entry"))
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_not_stopped() {
        let result = create_plan(
            &name(),
            State {
                container_is_stopped: false,
                ..droppable_state()
            },
        );

        assert!(matches!(
            result,
            Err(DropSeedlingError::CannotDropSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_to_drop_a_core_seedling() {
        let result = create_plan(
            &name(),
            State {
                origin: Some(labels::Origin::Core),
                ..droppable_state()
            },
        );

        assert!(matches!(result, Err(DropSeedlingError::CoreSeedling(_))));
    }
}
