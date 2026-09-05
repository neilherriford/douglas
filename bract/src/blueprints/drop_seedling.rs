use crate::blueprints::{
    agent_container_name, build_client, container_name, provision_seedling_secrets,
    seedling_network_name, traefik_dynamic_dir,
};
use crate::labels;
use async_trait::async_trait;
use blueprint::{
    Command, Step, push_step,
    bootstrap::{execute_plan, resolve_plan},
};
use config::DouglasFolders;
use docker::client::{ClientBuilder, ContainerRef};
use docker_types::DockerNameError;
use file_system::{FileDeleter, FileReader, FileSystemError, Folder, FolderDeleter};
use log::{Reporter, ScopeKind, Span};
use ram_disk::{RamDisk, RamDiskError};
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
    #[error("RAM disk error: {0}")]
    RamDiskError(#[from] RamDiskError),
}

struct Context<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    resin_client: &'a mut dyn resin_client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    file_deleter: &'a dyn FileDeleter,
    folder_deleter: &'a dyn FolderDeleter,
    douglas_folders: &'a DouglasFolders,
    ram_disk: &'a dyn RamDisk,
}

#[derive(Debug)]
struct State {
    container_exists: bool,
    container_is_stopped: bool,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
    origin: Option<seedbank_types::Origin>,
    network_exists: bool,
    seedbank_entry_exists: bool,
    route_exists: bool,
    mounts_dir_exists: bool,
    agent_container_exists: bool,
    agent_container_is_stopped: bool,
    agent_container_name: docker_types::ContainerName,
    agent_mount_is_ram_disk: bool,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client_builder: &dyn ClientBuilder,
    resin_client_builder: &dyn resin_client::ClientBuilder,
    seedbank_client: &dyn seedbank_client::Client,
    file_deleter: &dyn FileDeleter,
    folder_deleter: &dyn FolderDeleter,
    file_reader: &dyn FileReader,
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
    ram_disk: &dyn RamDisk,
    name: &seedbank_types::Name,
) -> Result<(), DropSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Dropping seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut docker_client = match build_client(
        docker_client_builder.build(Arc::clone(&reporter)),
        DropSeedlingError::FailedBoostrap,
    )
    .await
    {
        Ok(docker_client) => docker_client,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut resin_client = match build_client(
        resin_client_builder.build(Arc::clone(&reporter)),
        DropSeedlingError::FailedBoostrap,
    )
    .await
    {
        Ok(resin_client) => resin_client,
        Err(err) => return guard.finish(Err(err)),
    };

    let state = {
        let mut state_observer =
            StateObserver::new(&mut *docker_client, seedbank_client, file_reader, folder);
        state_observer
            .discover(guard.span(), douglas_folders, ram_disk, name)
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
            folder_deleter,
            douglas_folders,
            ram_disk,
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
    folder: &'a dyn Folder,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a mut dyn docker::client::Client,
        seedbank_client: &'a dyn seedbank_client::Client,
        file_reader: &'a dyn FileReader,
        folder: &'a dyn Folder,
    ) -> Self {
        Self {
            docker_client,
            seedbank_client,
            file_reader,
            folder,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
        ram_disk: &dyn RamDisk,
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
            mounts_dir_exists: self
                .folder
                .exists(&douglas_folders.seedling_mounts_dir(name.as_ref())),
            agent_container_exists: false,
            agent_container_is_stopped: false,
            agent_container_name: agent_container_name(name)?,
            agent_mount_is_ram_disk: ram_disk.is_mounted(
                &provision_seedling_secrets::agent_mount_dir(douglas_folders, name),
            )?,
        };

        result.origin =
            determine_origin(self.seedbank_client, result.seedbank_entry_exists, name).await?;

        result.network_exists = self
            .docker_client
            .network_exists(&seedling_network_name(name)?)
            .await?;

        result.agent_container_exists = self
            .docker_client
            .container_exists(ContainerRef::FullName(result.agent_container_name.clone()))
            .await?;
        if result.agent_container_exists {
            result.agent_container_is_stopped = matches!(
                self.docker_client
                    .container_status(ContainerRef::FullName(result.agent_container_name.clone()))
                    .await?,
                docker_types::Status::Created
                    | docker_types::Status::Exited
                    | docker_types::Status::Dead
            );
        }

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
        result.version = labels::get_version(&container_labels).ok();

        guard.finish(Ok(result))
    }
}

async fn determine_origin(
    seedbank_client: &dyn seedbank_client::Client,
    seedbank_entry_exists: bool,
    name: &seedbank_types::Name,
) -> Result<Option<seedbank_types::Origin>, DropSeedlingError> {
    if !seedbank_entry_exists {
        return Ok(None);
    }

    Ok(Some(seedbank_client.load(name).await?.definition.origin))
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    state: State,
) -> Result<Vec<Step<Context<'a>>>, DropSeedlingError> {
    let mut steps: Vec<Step<Context>> = Vec::new();

    if state.origin == Some(seedbank_types::Origin::Core) {
        return Err(DropSeedlingError::CoreSeedling(name.to_string()));
    }
    if state.container_exists && !state.container_is_stopped {
        return Err(DropSeedlingError::CannotDropSeedling(
            "Seedling is not stopped".to_string(),
        ));
    }
    if state.agent_container_exists && !state.agent_container_is_stopped {
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

    if state.agent_container_exists {
        push_step(
            &mut steps,
            DropSeedling::new(name.clone(), state.agent_container_name, None),
        );
    }

    if state.network_exists {
        push_step(&mut steps, DeleteSeedlingNetwork::new(name.clone()));
    }

    if state.seedbank_entry_exists {
        push_step(&mut steps, DeleteSeedbankEntry::new(name.clone()));
    }

    if state.agent_mount_is_ram_disk {
        push_step(&mut steps, UnmountAgentRamDisk::new(name.clone()));
    }

    if state.mounts_dir_exists {
        push_step(&mut steps, DeleteSeedlingMounts::new(name.clone()));
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

struct UnmountAgentRamDisk {
    seedling_name: seedbank_types::Name,
}

impl UnmountAgentRamDisk {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for UnmountAgentRamDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unmounting OpenBao agent ram disk for '{}'",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for UnmountAgentRamDisk {
    fn name(&self) -> String {
        "Unmounting OpenBao agent ram disk".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Unmounting OpenBao agent ram disk for '{}'…",
                    self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        let mount_dir = provision_seedling_secrets::agent_mount_dir(
            context.douglas_folders,
            &self.seedling_name,
        );
        context.ram_disk.unmount(&mount_dir)?;

        guard.finish(Ok(()))
    }
}

struct DeleteSeedlingMounts {
    seedling_name: seedbank_types::Name,
}

impl DeleteSeedlingMounts {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for DeleteSeedlingMounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Deleting mounts for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DeleteSeedlingMounts {
    fn name(&self) -> String {
        "Deleting mounts".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Deleting mounts for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let mounts_dir = context
            .douglas_folders
            .seedling_mounts_dir(self.seedling_name.as_ref());
        context.folder_deleter.delete(&mounts_dir)?;

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
    use std::{num::NonZeroU8, str::FromStr};

    use seedbank_types::{HealthCheck, HealthCheckCommand};

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
            origin: Some(seedbank_types::Origin::User),
            network_exists: true,
            seedbank_entry_exists: true,
            route_exists: true,
            mounts_dir_exists: true,
            agent_container_exists: false,
            agent_container_is_stopped: false,
            agent_container_name: agent_container_name(&name()).unwrap(),
            agent_mount_is_ram_disk: false,
        }
    }

    fn step_descriptions(steps: Vec<Step<Context<'_>>>) -> Vec<String> {
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
                "Deleting mounts for 'hello-world'".to_string(),
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
                "Deleting mounts for 'hello-world'".to_string(),
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
    fn test_create_plan_should_skip_the_mounts_step_when_no_mounts_dir_exists() {
        let steps = create_plan(
            &name(),
            State {
                mounts_dir_exists: false,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert!(
            !step_descriptions(steps)
                .iter()
                .any(|description| description.contains("mounts"))
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
                origin: Some(seedbank_types::Origin::Core),
                ..droppable_state()
            },
        );

        assert!(matches!(result, Err(DropSeedlingError::CoreSeedling(_))));
    }

    #[test]
    fn test_create_plan_should_also_drop_an_existing_agent_container() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_stopped: true,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Removing traefik route for 'hello-world'".to_string(),
                "Dropping seedling 'hello-world' (v1)".to_string(),
                "Dropping seedling 'hello-world'".to_string(),
                "Deleting network for 'hello-world'".to_string(),
                "Deleting seedbank entry for 'hello-world'".to_string(),
                "Deleting mounts for 'hello-world'".to_string(),
                "Releasing default for 'hello-world'".to_string(),
                "Deleting resin repository for 'hello-world'".to_string(),
            ]
        );
    }

    #[test]
    fn test_create_plan_should_unmount_the_agent_ram_disk_before_deleting_mounts_when_present() {
        let steps = create_plan(
            &name(),
            State {
                agent_mount_is_ram_disk: true,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        let descriptions = step_descriptions(steps);
        let unmount_index = descriptions
            .iter()
            .position(|description| description.contains("agent ram disk"))
            .expect("should include the unmount step");
        let delete_mounts_index = descriptions
            .iter()
            .position(|description| description == "Deleting mounts for 'hello-world'")
            .expect("should include the delete mounts step");

        assert!(unmount_index < delete_mounts_index);
    }

    #[test]
    fn test_create_plan_should_skip_the_ram_disk_unmount_step_when_not_ram_backed() {
        let steps = create_plan(&name(), droppable_state()).expect("should produce a plan");

        assert!(
            !step_descriptions(steps)
                .iter()
                .any(|description| description.contains("agent ram disk"))
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_agent_container_is_not_stopped() {
        let result = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_stopped: false,
                ..droppable_state()
            },
        );

        assert!(matches!(
            result,
            Err(DropSeedlingError::CannotDropSeedling(_))
        ));
    }

    fn seedling(origin: seedbank_types::Origin) -> seedbank_types::Seedling {
        seedbank_types::Seedling {
            id: seedbank_types::Id { value: 0 },
            name: name(),
            version: seedbank_types::Version(1),
            definition: seedbank_types::SeedlingDefinition::new(
                docker_types::VersionedImageName::specific("hello-world", "1"),
                std::collections::HashMap::new(),
                seedbank_types::Routing::None,
                HealthCheck {
                    command: HealthCheckCommand::from_str("true").unwrap(),
                    wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
                },
            )
            .with_origin(origin),
        }
    }

    #[tokio::test]
    async fn test_determine_origin_should_be_none_when_the_seedbank_entry_does_not_exist() {
        let seedbank_client = seedbank_client::MockClient::new();

        let result = determine_origin(&seedbank_client, false, &name())
            .await
            .expect("should determine origin");

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_determine_origin_should_load_the_persisted_origin_for_a_core_seedling() {
        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_load()
            .returning(|_| Ok(seedling(seedbank_types::Origin::Core)));

        let result = determine_origin(&seedbank_client, true, &name())
            .await
            .expect("should determine origin");

        assert_eq!(result, Some(seedbank_types::Origin::Core));
    }

    #[tokio::test]
    async fn test_determine_origin_should_load_the_persisted_origin_for_a_user_seedling() {
        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_load()
            .returning(|_| Ok(seedling(seedbank_types::Origin::User)));

        let result = determine_origin(&seedbank_client, true, &name())
            .await
            .expect("should determine origin");

        assert_eq!(result, Some(seedbank_types::Origin::User));
    }
}
