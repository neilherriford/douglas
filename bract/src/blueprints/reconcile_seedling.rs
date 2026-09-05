use crate::{
    blueprints::{
        AGENT_MOUNT_RAM_DISK_SIZE_MB, EXPECTED_MOUNT_MODE, SYSTEM_NETWORK_NAME,
        agent_container_name, container_name, provision_seedling_secrets, seedling_network_name,
    },
    labels::{self},
    rolodex::{Rolodex, RolodexError},
};
use async_trait::async_trait;
use blueprint::{
    Command, Step, push_step,
    bootstrap::{execute_plan, resolve_plan},
};
use config::DouglasFolders;
use credentials::Credentials;
use docker::{
    DockerError,
    client::{ClientBuilder, ContainerRef, ImageRef},
};
use docker_types::{
    ContainerName, ContainerUser, DockerNameError, MountDefinition, NewContainer,
    VersionedImageName,
};
use file_system::{
    FileReader, FileSystemError, FileWriter, Folder, Inspect, Modes, Permissions, RelativePath,
    path_to_string,
};
use log::{Level, Reporter, ScopeKind, Span};
use ram_disk::RamDisk;
use seedbank_types::{MountContents, MountFile};
use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ReconcileSeedlingError {
    #[error("Resin error: {0}")]
    ResinError(#[from] resin_client::Error),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Label error: {0}")]
    LabelError(#[from] labels::LabelError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Rolodex error {0}")]
    RolodexError(#[from] RolodexError),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Could not locate the image to build the seedling")]
    MissingImage,
    #[error("Service account missing")]
    MissingServiceAccount,
    #[error("Share group missing")]
    MissingShareGroup,
    #[error("Invalid mount path {0}")]
    InvalidMountPath(RelativePath),
    #[error("Seedbank error {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Refusing to downgrade: the running container is newer than the declared version")]
    WouldDowngrade,
    #[error("Agent provisioning was requested but not available")]
    MissingAgentProvisioning,
}

struct Context<'a> {
    name: &'a seedbank_types::Name,
    version: &'a seedbank_types::Version,
    seedling_definition: &'a seedbank_types::SeedlingDefinition,
    agent_provisioning: Option<&'a provision_seedling_secrets::AgentProvisioning>,
    credentials: &'a dyn Credentials,
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    douglas_folders: &'a DouglasFolders,
    rolodex: &'a dyn Rolodex,
    folder: &'a dyn Folder,
    file_writer: &'a dyn FileWriter,
    permissions: &'a dyn Permissions,
    registry: &'a docker_types::Registry,
    ram_disk: &'a dyn RamDisk,
}

#[derive(Debug)]
struct State {
    seedling_version: VersionComparison,
    container_name: ContainerName,
    image_status: ImageStatus,
    container_status: VersionComparison,
    container_version: Option<seedbank_types::Version>,
    container_is_running: bool,
    missing_service_credentials: bool,
    missing_share_groups: Vec<(seedbank_types::Name, Vec<String>)>,
    missing_mount_folders: Vec<PathBuf>,
    needs_mount_ownership: Vec<(PathBuf, seedbank_types::Name, seedbank_types::MountType)>,
    missing_mount_mode: Vec<(PathBuf, Modes)>,
    missing_mount_files: Vec<(PathBuf, String, Vec<u8>)>,
    agent_missing_service_credentials: bool,
    agent_container_exists: bool,
    agent_container_is_running: bool,
}

#[derive(Default, Debug, PartialEq, Eq)]
enum ImageStatus {
    #[default]
    Unknown,
    Local,
    AvailableFromResin,
}

#[derive(Default, Debug)]
enum VersionComparison {
    #[default]
    Missing,
    Current,
    Stale,
    Newer,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    credentials: &dyn Credentials,
    inspect: &dyn Inspect,
    folder: &dyn Folder,
    file_reader: &dyn FileReader,
    file_writer: &dyn FileWriter,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
    docker_client_builder: &dyn ClientBuilder,
    resin_client_builder: &dyn resin_client::ClientBuilder,
    seedbank_client: &dyn seedbank_client::Client,
    registry: &docker_types::Registry,
    rolodex: &dyn Rolodex,
    name: &seedbank_types::Name,
    version: &seedbank_types::Version,
    seedling_definition: &seedbank_types::SeedlingDefinition,
    agent_provisioning: Option<&provision_seedling_secrets::AgentProvisioning>,
    ram_disk: &dyn RamDisk,
) -> Result<(), ReconcileSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Planning {name} seedling creation…"),
        log::ScopeKind::Group,
    )
    .start_guard();
    let mut docker_client = match docker_client_builder.build(Arc::clone(&reporter)).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(ReconcileSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let mut resin_client = match resin_client_builder.build(Arc::clone(&reporter)).await {
        Ok(resin_client) => resin_client,
        Err(err) => {
            return guard.finish(Err(ReconcileSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let state = {
        let mut state_observer = StateObserver::new(
            &mut *docker_client,
            seedbank_client,
            &mut *resin_client,
            rolodex,
            douglas_folders,
            folder,
            inspect,
            file_reader,
            permissions,
            registry,
        );
        state_observer
            .discover(
                guard.span(),
                name,
                version,
                seedling_definition,
                agent_provisioning.is_some(),
            )
            .await?
    };

    let plan = match resolve_plan(
        guard.span(),
        create_plan(name, version, seedling_definition, state, registry),
    ) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            name,
            version,
            seedling_definition,
            agent_provisioning,
            credentials,
            docker_client: &mut *docker_client,
            seedbank_client,
            rolodex,
            douglas_folders,
            folder,
            permissions,
            file_writer,
            registry,
            ram_disk,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            ReconcileSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    resin_client: &'a mut dyn resin_client::Client,
    rolodex: &'a dyn Rolodex,
    douglas_folders: &'a DouglasFolders,
    folder: &'a dyn Folder,
    inspect: &'a dyn Inspect,
    file_reader: &'a dyn FileReader,
    permissions: &'a dyn Permissions,
    registry: &'a docker_types::Registry,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a mut dyn docker::client::Client,
        seedbank_client: &'a dyn seedbank_client::Client,
        resin_client: &'a mut dyn resin_client::Client,
        rolodex: &'a dyn Rolodex,
        douglas_folders: &'a DouglasFolders,
        folder: &'a dyn Folder,
        inspect: &'a dyn Inspect,
        file_reader: &'a dyn FileReader,
        permissions: &'a dyn Permissions,
        registry: &'a docker_types::Registry,
    ) -> Self {
        Self {
            docker_client,
            seedbank_client,
            resin_client,
            rolodex,
            douglas_folders,
            folder,
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
        version: &seedbank_types::Version,
        seedling_definition: &seedbank_types::SeedlingDefinition,
        agent_requested: bool,
    ) -> Result<State, ReconcileSeedlingError> {
        let guard = span
            .create_child(
                "Starting bract system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State {
            seedling_version: VersionComparison::Missing,
            container_name: container_name(name)?,
            image_status: ImageStatus::Unknown,
            container_status: VersionComparison::Missing,
            container_version: None,
            container_is_running: false,
            missing_service_credentials: true,
            missing_share_groups: Vec::new(),
            missing_mount_folders: Vec::new(),
            needs_mount_ownership: Vec::new(),
            missing_mount_mode: Vec::new(),
            missing_mount_files: Vec::new(),
            agent_missing_service_credentials: true,
            agent_container_exists: false,
            agent_container_is_running: false,
        };

        if agent_requested {
            let agent_account = provision_seedling_secrets::agent_account_full_name(name);
            result.agent_missing_service_credentials =
                self.rolodex.find_service_account(&agent_account)?.is_none();

            let agent_container = agent_container_name(name)?;
            result.agent_container_exists = self
                .docker_client
                .container_exists(ContainerRef::FullName(agent_container.clone()))
                .await?;
            if result.agent_container_exists {
                result.agent_container_is_running =
                    self.discover_container_is_running(&agent_container).await?;
            }
        }

        result.seedling_version = self
            .discover_current_seedbank_version(name, version)
            .await?;
        guard.span().message(
            Level::Info,
            &format!("Seedbank definition status: {:?}", result.seedling_version),
        );

        result.image_status = self
            .discover_image_status(&seedling_definition.image)
            .await?;
        guard.span().message(
            Level::Info,
            &format!("Image status: {:?}", result.image_status),
        );

        (result.container_status, result.container_version) = self
            .discover_container_status(&result.container_name, version)
            .await?;
        guard.span().message(
            Level::Info,
            &format!("Container version status: {:?}", result.container_status),
        );

        result.container_is_running = self
            .discover_container_is_running(&result.container_name)
            .await?;
        guard.span().message(
            Level::Info,
            &format!("Container running: {}", result.container_is_running),
        );

        result.missing_service_credentials =
            self.rolodex.find_service_account(name.as_ref())?.is_none();
        guard.span().message(
            Level::Info,
            &format!(
                "Service credentials missing: {}",
                result.missing_service_credentials
            ),
        );

        for (mount_name, mount) in &seedling_definition.mounts {
            self.discover_missing_mount(&mut result, name, mount_name, mount)?;
        }
        guard.span().message(
            Level::Info,
            &format!(
                "Mounts: {} folder(s) missing, {} needing ownership, {} needing mode, {} file(s) missing/stale",
                result.missing_mount_folders.len(),
                result.needs_mount_ownership.len(),
                result.missing_mount_mode.len(),
                result.missing_mount_files.len(),
            ),
        );

        guard.finish(Ok(result))
    }

    fn discover_missing_mount(
        &mut self,
        result: &mut State,
        seedling_name: &seedbank_types::Name,
        name: &seedbank_types::Name,
        mount: &seedbank_types::Mount,
    ) -> Result<(), ReconcileSeedlingError> {
        if let seedbank_types::MountType::PersistedShared(siblings) = mount.kind()
            && self
                .rolodex
                .find_share_group(seedling_name.as_ref(), name.as_ref())?
                .is_none()
        {
            result.missing_share_groups.push((
                name.clone(),
                siblings
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            ));
        }

        let root_mount_path = self.mount_path(seedling_name, name);
        if self.folder.exists(&root_mount_path) {
            match self.expected_mount_group_name(seedling_name, name, mount)? {
                Some(expected_group) => {
                    let (_actual_user, actual_group) = self
                        .permissions
                        .get_user_and_group_ownership(&root_mount_path)?;
                    if actual_group != expected_group {
                        result.needs_mount_ownership.push((
                            root_mount_path.clone(),
                            name.clone(),
                            mount.kind().clone(),
                        ));
                    }
                }
                None => {
                    result.needs_mount_ownership.push((
                        root_mount_path.clone(),
                        name.clone(),
                        mount.kind().clone(),
                    ));
                }
            }
            let actual_mode = self.permissions.get_mode(&root_mount_path)?;
            if actual_mode != EXPECTED_MOUNT_MODE {
                result
                    .missing_mount_mode
                    .push((root_mount_path.clone(), EXPECTED_MOUNT_MODE));
            }
            self.discover_missing_mount_contents(result, mount, &root_mount_path)?;
        } else {
            self.add_missing_mount(result, name, mount, &root_mount_path)?;
        }
        Ok(())
    }

    fn expected_mount_group_name(
        &self,
        seedling_name: &seedbank_types::Name,
        mount_name: &seedbank_types::Name,
        mount: &seedbank_types::Mount,
    ) -> Result<Option<String>, ReconcileSeedlingError> {
        match mount.kind() {
            seedbank_types::MountType::PersistedShared(_) => Ok(self
                .rolodex
                .find_share_group(seedling_name.as_ref(), mount_name.as_ref())?
                .map(|credential| credential.system_name)),
            seedbank_types::MountType::Persisted | seedbank_types::MountType::InMemory => Ok(self
                .rolodex
                .find_service_account(seedling_name.as_ref())?
                .map(|account| account.group.system_name)),
        }
    }

    fn discover_missing_mount_contents(
        &mut self,
        result: &mut State,
        mount: &seedbank_types::Mount,
        root_mount_path: &Path,
    ) -> Result<(), ReconcileSeedlingError> {
        for content in mount.contents() {
            match content {
                MountContents::FolderOnly(relative_path) => {
                    let mut path = root_mount_path.to_path_buf();
                    path.push(relative_path);
                    if !self.inspect.exists(&path) {
                        result.missing_mount_folders.push(path);
                    }
                }
                MountContents::File(mount_file) => {
                    let mut full_path = root_mount_path.to_path_buf();
                    full_path.push(mount_file.file_relative_path.clone());

                    if self.inspect.exists(&full_path) {
                        let actual_bytes = self.file_reader.read_all_bytes(&full_path)?;

                        if mount_file.contents != actual_bytes {
                            result
                                .missing_mount_files
                                .push(self.create_missing_mount_file(mount_file, &full_path)?);
                        }
                    } else {
                        if let Some(parent) = self.folder.parent(&full_path) {
                            result.missing_mount_folders.push(parent);
                            result
                                .missing_mount_files
                                .push(self.create_missing_mount_file(mount_file, &full_path)?);
                        } else {
                            return Err(ReconcileSeedlingError::FileSystemError(
                                FileSystemError::InvalidPath(full_path),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn create_missing_mount_file(
        &self,
        mount_file: &MountFile,
        full_path: &Path,
    ) -> Result<(PathBuf, String, Vec<u8>), ReconcileSeedlingError> {
        Ok((
            self.folder
                .parent(full_path)
                .ok_or(ReconcileSeedlingError::InvalidMountPath(
                    mount_file.file_relative_path.clone(),
                ))?,
            self.folder
                .pop(full_path)
                .ok_or(ReconcileSeedlingError::InvalidMountPath(
                    mount_file.file_relative_path.clone(),
                ))?,
            mount_file.contents.clone(),
        ))
    }

    async fn discover_image_status(
        &mut self,
        image: &docker_types::VersionedImageName,
    ) -> Result<ImageStatus, ReconcileSeedlingError> {
        if self
            .docker_client
            .image_exists(self.registry, ImageRef::VersionedName(image.clone()))
            .await?
        {
            Ok(ImageStatus::Local)
        } else if self.resin_client.image_exists(image).await? {
            Ok(ImageStatus::AvailableFromResin)
        } else {
            Ok(ImageStatus::Unknown)
        }
    }

    async fn discover_container_status(
        &self,
        container_name: &ContainerName,
        version: &seedbank_types::Version,
    ) -> Result<(VersionComparison, Option<seedbank_types::Version>), ReconcileSeedlingError> {
        match self
            .docker_client
            .container_labels(ContainerRef::FullName(container_name.clone()))
            .await
        {
            Ok(labels) => {
                let actual_version = labels::get_version(&labels)?;

                let comparison = match version.cmp(&actual_version) {
                    Ordering::Less => VersionComparison::Newer,
                    Ordering::Equal => VersionComparison::Current,
                    Ordering::Greater => VersionComparison::Stale,
                };
                Ok((comparison, Some(actual_version)))
            }
            Err(DockerError::ResourceNotFound) => Ok((VersionComparison::Missing, None)),
            Err(err) => Err(ReconcileSeedlingError::DockerError(err)),
        }
    }

    fn mount_path(
        &self,
        seedling_name: &seedbank_types::Name,
        name: &seedbank_types::Name,
    ) -> PathBuf {
        self.douglas_folders
            .seedling_mount(seedling_name.as_ref(), name.as_ref())
    }

    fn add_missing_mount(
        &self,
        result: &mut State,
        name: &seedbank_types::Name,
        mount: &seedbank_types::Mount,
        root_mount_path: &Path,
    ) -> Result<(), ReconcileSeedlingError> {
        result
            .missing_mount_folders
            .push(root_mount_path.to_path_buf());
        result.needs_mount_ownership.push((
            root_mount_path.to_path_buf(),
            name.clone(),
            mount.kind().clone(),
        ));
        result
            .missing_mount_mode
            .push((root_mount_path.to_path_buf(), EXPECTED_MOUNT_MODE));
        for content in mount.contents() {
            match content {
                MountContents::FolderOnly(relative_path) => {
                    let mut path = root_mount_path.to_path_buf();
                    path.push(relative_path);
                    result.missing_mount_folders.push(path);
                }
                MountContents::File(mount_file) => {
                    let mut full_path = root_mount_path.to_path_buf();
                    full_path.push(mount_file.file_relative_path.clone());

                    if let Some(parent) = self.folder.parent(&full_path) {
                        result.missing_mount_folders.push(parent);
                        result
                            .missing_mount_files
                            .push(self.create_missing_mount_file(mount_file, &full_path)?);
                    } else {
                        return Err(ReconcileSeedlingError::FileSystemError(
                            FileSystemError::InvalidPath(full_path),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    async fn discover_container_is_running(
        &self,
        container_name: &ContainerName,
    ) -> Result<bool, ReconcileSeedlingError> {
        match self
            .docker_client
            .container_status(ContainerRef::FullName(container_name.clone()))
            .await
        {
            Ok(status) => Ok(status == docker_types::Status::Running),
            Err(DockerError::ResourceNotFound) => Ok(false),
            Err(err) => Err(ReconcileSeedlingError::DockerError(err)),
        }
    }

    async fn discover_current_seedbank_version(
        &self,
        name: &seedbank_types::Name,
        version: &seedbank_types::Version,
    ) -> Result<VersionComparison, ReconcileSeedlingError> {
        Ok(match self.seedbank_client.status(name).await? {
            seedbank_types::SeedlingStatus::Defined(current_version) => {
                match current_version.cmp(version) {
                    Ordering::Less => VersionComparison::Stale,
                    Ordering::Equal => VersionComparison::Current,
                    Ordering::Greater => VersionComparison::Newer,
                }
            }
            seedbank_types::SeedlingStatus::Unknown => VersionComparison::Missing,
        })
    }
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    version: &seedbank_types::Version,
    seedling_definition: &seedbank_types::SeedlingDefinition,
    mut state: State,
    registry: &docker_types::Registry,
) -> Result<Vec<Step<Context<'a>>>, ReconcileSeedlingError> {
    let mut steps: Vec<Step<Context>> = Vec::new();

    match state.seedling_version {
        VersionComparison::Missing => push_step(
            &mut steps,
            CreateSeedling::new(name.clone(), version.clone(), seedling_definition.clone()),
        ),
        VersionComparison::Current | VersionComparison::Newer => {}
        VersionComparison::Stale => push_step(
            &mut steps,
            UpdateSeedling::new(name.clone(), version.clone(), seedling_definition.clone()),
        ),
    }

    if state.missing_service_credentials {
        push_step(&mut steps, CreateServiceAccount::new(name.clone()));
    }

    let agent_requested = seedling_definition.secrets.is_some();

    if agent_requested && state.agent_missing_service_credentials {
        push_step(&mut steps, CreateAgentServiceAccount::new(name.clone()));
    }

    for (mount_name, guest_names) in state.missing_share_groups {
        push_step(
            &mut steps,
            CreateShareGroup::new(name.clone(), mount_name, guest_names),
        );
    }

    for missing_mount_folder in state.missing_mount_folders {
        push_step(&mut steps, CreateMountFolder::new(missing_mount_folder));
    }

    for (path, mount_name, kind) in state.needs_mount_ownership {
        push_step(&mut steps, SetMountOwnership::new(path, mount_name, kind));
    }

    for (path, mode) in state.missing_mount_mode {
        push_step(&mut steps, SetMountMode::new(path, mode));
    }

    for (parrent_path, file_name, contents) in state.missing_mount_files.drain(std::ops::RangeFull)
    {
        push_step(
            &mut steps,
            WriteFile::new(parrent_path, file_name, contents),
        );
    }

    match state.image_status {
        ImageStatus::Unknown => return Err(ReconcileSeedlingError::MissingImage),
        ImageStatus::Local => {}
        ImageStatus::AvailableFromResin => push_step(
            &mut steps,
            PullImageFromResin::new(registry, seedling_definition.image.clone()),
        ),
    }

    if agent_requested {
        push_step(&mut steps, EnsureAgentMount::new(name.clone()));

        if !state.agent_container_exists {
            push_step(
                &mut steps,
                BuildAgentContainer::new(name.clone(), version.clone()),
            );
            push_step(
                &mut steps,
                StartContainer::new(agent_container_name(name)?, version.clone()),
            );
        } else if !state.agent_container_is_running {
            push_step(
                &mut steps,
                StartContainer::new(agent_container_name(name)?, version.clone()),
            );
        }
    }

    let container_name = state.container_name;

    match state.container_status {
        VersionComparison::Missing => {
            push_step(
                &mut steps,
                BuildContainer::new(name.clone(), version.clone()),
            );
            push_step(
                &mut steps,
                StartContainer::new(container_name, version.clone()),
            );
        }
        VersionComparison::Current => {
            if !state.container_is_running {
                push_step(
                    &mut steps,
                    StartContainer::new(container_name, version.clone()),
                );
            }
        }
        VersionComparison::Stale => {
            let current_version = state
                .container_version
                .expect("stale container status implies a known container version");

            if state.container_is_running {
                push_step(
                    &mut steps,
                    StopContainer::new(container_name.clone(), current_version.clone()),
                );
            }
            push_step(
                &mut steps,
                DropContainer::new(container_name.clone(), current_version),
            );
            push_step(
                &mut steps,
                BuildContainer::new(name.clone(), version.clone()),
            );
            push_step(
                &mut steps,
                StartContainer::new(container_name, version.clone()),
            );
        }
        VersionComparison::Newer => return Err(ReconcileSeedlingError::WouldDowngrade),
    }

    Ok(steps)
}

struct CreateSeedling {
    name: seedbank_types::Name,
    version: seedbank_types::Version,
    seedling_definition: seedbank_types::SeedlingDefinition,
}

impl CreateSeedling {
    pub fn new(
        name: seedbank_types::Name,
        version: seedbank_types::Version,
        seedling_definition: seedbank_types::SeedlingDefinition,
    ) -> Self {
        Self {
            name,
            version,
            seedling_definition,
        }
    }
}

impl std::fmt::Display for CreateSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Creating seedling for '{}' version '{}'",
            self.name, self.version
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateSeedling {
    fn name(&self) -> String {
        "Creating seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Creating seedling for '{}' version '{}'",
                    self.name, self.version
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .create(&self.name, &self.version, &self.seedling_definition)
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct UpdateSeedling {
    name: seedbank_types::Name,
    version: seedbank_types::Version,
    seedling_definition: seedbank_types::SeedlingDefinition,
}

impl UpdateSeedling {
    pub fn new(
        name: seedbank_types::Name,
        version: seedbank_types::Version,
        seedling_definition: seedbank_types::SeedlingDefinition,
    ) -> Self {
        Self {
            name,
            version,
            seedling_definition,
        }
    }
}

impl std::fmt::Display for UpdateSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Updating seedling for '{}' version '{}'",
            self.name, self.version
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for UpdateSeedling {
    fn name(&self) -> String {
        "Updating seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Updating seedling for '{}' version '{}'",
                    self.name, self.version
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .update(&self.name, &self.version, &self.seedling_definition)
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct CreateServiceAccount {
    name: seedbank_types::Name,
}

impl CreateServiceAccount {
    pub fn new(name: seedbank_types::Name) -> Self {
        Self { name }
    }
}

impl std::fmt::Display for CreateServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Creating service account '{}'", self.name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateServiceAccount {
    fn name(&self) -> String {
        "Creating service account".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Creating service account '{}'…", self.name),
                ScopeKind::Step,
            )
            .start_guard();

        context.rolodex.create_service_account(self.name.as_ref())?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct CreateAgentServiceAccount {
    seedling_name: seedbank_types::Name,
}

impl CreateAgentServiceAccount {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for CreateAgentServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Creating OpenBao agent service account for '{}'",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateAgentServiceAccount {
    fn name(&self) -> String {
        "Creating OpenBao agent service account".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Creating OpenBao agent service account for '{}'…",
                    self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        let account_name = provision_seedling_secrets::agent_account_full_name(&self.seedling_name);
        context.rolodex.create_service_account(&account_name)?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct CreateMountFolder {
    path: PathBuf,
}

impl CreateMountFolder {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl std::fmt::Display for CreateMountFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Creating seedling mount '{}'",
            path_to_string(&self.path)
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateMountFolder {
    fn name(&self) -> String {
        "Creating seedling mount".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Creating seedling mount '{}'…", path_to_string(&self.path)),
                ScopeKind::Step,
            )
            .start_guard();

        context.folder.create_recursively(&self.path)?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct SetMountOwnership {
    path: PathBuf,
    mount_name: seedbank_types::Name,
    kind: seedbank_types::MountType,
}

impl SetMountOwnership {
    pub fn new(
        path: PathBuf,
        mount_name: seedbank_types::Name,
        kind: seedbank_types::MountType,
    ) -> Self {
        Self {
            path,
            mount_name,
            kind,
        }
    }
}

impl std::fmt::Display for SetMountOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Setting mount ownership '{}'",
            path_to_string(&self.path)
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for SetMountOwnership {
    fn name(&self) -> String {
        "Setting mount ownership".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Setting mount ownership '{}'…", path_to_string(&self.path)),
                ScopeKind::Step,
            )
            .start_guard();

        let Some(service_account) = context
            .rolodex
            .find_service_account(context.name.as_ref())?
        else {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(ReconcileSeedlingError::MissingServiceAccount));
        };

        let group_system_name = match &self.kind {
            seedbank_types::MountType::PersistedShared(_) => {
                let Some(credential) = context
                    .rolodex
                    .find_share_group(context.name.as_ref(), self.mount_name.as_ref())?
                else {
                    guard.finish_with_outcome(log::Outcome::Failed);
                    return Err(Box::new(ReconcileSeedlingError::MissingShareGroup));
                };
                credential.system_name
            }
            seedbank_types::MountType::Persisted | seedbank_types::MountType::InMemory => {
                service_account.group.system_name.clone()
            }
        };

        context.permissions.change_user_and_group_ownership(
            &self.path,
            &service_account.user.system_name,
            &group_system_name,
        )?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct CreateShareGroup {
    seedling_name: seedbank_types::Name,
    mount_name: seedbank_types::Name,
    guest_names: Vec<String>,
}

impl CreateShareGroup {
    pub fn new(
        seedling_name: seedbank_types::Name,
        mount_name: seedbank_types::Name,
        guest_names: Vec<String>,
    ) -> Self {
        Self {
            seedling_name,
            mount_name,
            guest_names,
        }
    }
}

impl std::fmt::Display for CreateShareGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Creating share group '{}' for '{}'",
            self.mount_name, self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateShareGroup {
    fn name(&self) -> String {
        "Creating share group".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Creating share group '{}' for '{}'…",
                    self.mount_name, self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context.rolodex.create_share_group(
            self.seedling_name.as_ref(),
            self.mount_name.as_ref(),
            &self.guest_names,
        )?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct WriteFile {
    parent_path: PathBuf,
    file_name: String,
    contents: Vec<u8>,
}

impl WriteFile {
    pub fn new(parent_path: PathBuf, file_name: String, contents: Vec<u8>) -> Self {
        Self {
            parent_path,
            file_name,
            contents,
        }
    }
}

impl std::fmt::Display for WriteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Writing mount file '{}'", self.file_name,)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for WriteFile {
    fn name(&self) -> String {
        "Writing mount file".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Writing mount file '{}'", self.file_name),
                ScopeKind::Step,
            )
            .start_guard();

        context.folder.create_recursively(&self.parent_path)?;
        let mut file_path = self.parent_path.clone();
        file_path.push(self.file_name.clone());
        context
            .file_writer
            .write_all_bytes(&file_path, &self.contents)?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct SetMountMode {
    path: PathBuf,
    mode: Modes,
}

impl SetMountMode {
    pub fn new(path: PathBuf, mode: Modes) -> Self {
        Self { path, mode }
    }
}

impl std::fmt::Display for SetMountMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Setting mode on '{}' to '{}'",
            path_to_string(&self.path),
            self.mode
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for SetMountMode {
    fn name(&self) -> String {
        "Setting mode".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Setting mode on '{}' to '{}'…",
                    path_to_string(&self.path),
                    self.mode
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context.permissions.change_mode(&self.path, &self.mode)?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct EnsureAgentMount {
    seedling_name: seedbank_types::Name,
}

impl EnsureAgentMount {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for EnsureAgentMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ensuring OpenBao agent mount for '{}'",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for EnsureAgentMount {
    fn name(&self) -> String {
        "Ensuring OpenBao agent mount".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Ensuring OpenBao agent mount for '{}'…", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let Some(agent_provisioning) = context.agent_provisioning else {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(ReconcileSeedlingError::MissingAgentProvisioning));
        };

        context
            .folder
            .create_recursively(&agent_provisioning.mount_dir)?;
        let swap_protected = context
            .ram_disk
            .mount(&agent_provisioning.mount_dir, AGENT_MOUNT_RAM_DISK_SIZE_MB)?;
        if !swap_protected {
            guard.span().message(
                log::Level::Warn,
                &format!(
                    "'{}' agent credentials are on a RAM disk without swap protection (kernel predates tmpfs's noswap option) — they could be paged out to a real disk under memory pressure",
                    self.seedling_name
                ),
            );
        }

        let account_name = provision_seedling_secrets::agent_account_full_name(&self.seedling_name);
        let Some(service_account) = context.rolodex.find_service_account(&account_name)? else {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(ReconcileSeedlingError::MissingServiceAccount));
        };
        context.permissions.change_user_and_group_ownership(
            &agent_provisioning.mount_dir,
            &service_account.user.system_name,
            &service_account.group.system_name,
        )?;
        context
            .permissions
            .change_mode(&agent_provisioning.mount_dir, &EXPECTED_MOUNT_MODE)?;

        context.file_writer.write_all_bytes(
            &agent_provisioning.role_id_path,
            agent_provisioning.role_id.as_bytes(),
        )?;
        context.file_writer.write_all_bytes(
            &agent_provisioning.secret_id_path,
            agent_provisioning.secret_id.as_bytes(),
        )?;
        context.file_writer.write_all_bytes(
            &agent_provisioning.agent_config_path,
            &agent_provisioning.agent_config,
        )?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct PullImageFromResin {
    registry: docker_types::Registry,
    name: VersionedImageName,
}

impl PullImageFromResin {
    pub fn new(registry: &docker_types::Registry, name: VersionedImageName) -> Self {
        Self {
            registry: registry.clone(),
            name,
        }
    }
}

impl std::fmt::Display for PullImageFromResin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pull image '{}' from Resin", self.name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for PullImageFromResin {
    fn name(&self) -> String {
        "Pulling image".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Pulling image '{}' from resin…", self.name),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client
            .pull_image(&self.registry, &self.name)
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct StopContainer {
    name: ContainerName,
    version: seedbank_types::Version,
}

impl StopContainer {
    pub fn new(name: ContainerName, version: seedbank_types::Version) -> Self {
        Self { name, version }
    }
}

impl std::fmt::Display for StopContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stopping container '{}' (v{})", self.name, self.version)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StopContainer {
    fn name(&self) -> String {
        "Stopping container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Stopping container '{}' (v{})…", self.name, self.version),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client
            .stop_container(ContainerRef::FullName(self.name.clone()))
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct DropContainer {
    name: ContainerName,
    version: seedbank_types::Version,
}

impl DropContainer {
    pub fn new(name: ContainerName, version: seedbank_types::Version) -> Self {
        Self { name, version }
    }
}

impl std::fmt::Display for DropContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dropping container '{}' (v{})", self.name, self.version)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DropContainer {
    fn name(&self) -> String {
        "Dropping container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Dropping container '{}' (v{})…", self.name, self.version),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client
            .delete_container(ContainerRef::FullName(self.name.clone()))
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

fn published_ports_for(
    seedling_definition: &seedbank_types::SeedlingDefinition,
) -> Vec<docker_types::PortMapping> {
    let routed_additional_ports = match &seedling_definition.routing {
        seedbank_types::Routing::Routed { ports, .. } => ports.additional.as_slice(),
        seedbank_types::Routing::None => &[],
    };

    seedling_definition
        .published_ports
        .iter()
        .chain(routed_additional_ports)
        .map(|port| docker_types::PortMapping {
            host_port: port.external,
            container_port: port.internal,
        })
        .collect()
}

fn build_new_container(
    name: ContainerName,
    seedling_definition: &seedbank_types::SeedlingDefinition,
    uid: u32,
    gid: u32,
    mounts: Vec<MountDefinition>,
    version: &seedbank_types::Version,
    environment_variables: Vec<docker_types::EnvironmentVariable>,
) -> NewContainer {
    NewContainer {
        name,
        run_as: Some(ContainerUser {
            user_id: uid,
            group_id: gid,
        }),
        command: seedling_definition.command.clone(),
        environment_variables,
        image: seedling_definition.image.clone(),
        mounts,
        added_capabilities: seedling_definition.added_capabilities.clone(),
        labels: vec![
            labels::create_version_label(version),
            labels::create_origin_label(seedling_definition.origin),
        ],
        published_ports: published_ports_for(seedling_definition),
    }
}

struct BuildContainer {
    name: seedbank_types::Name,
    version: seedbank_types::Version,
}

impl BuildContainer {
    pub fn new(name: seedbank_types::Name, version: seedbank_types::Version) -> Self {
        Self { name, version }
    }
}

impl std::fmt::Display for BuildContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Building container '{}' (v{})", self.name, self.version)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for BuildContainer {
    fn name(&self) -> String {
        "Building container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Building container '{}' (v{})…", self.name, self.version),
                ScopeKind::Step,
            )
            .start_guard();

        let new_container_name: ContainerName = container_name(&self.name)?;
        let service_account = context
            .rolodex
            .find_service_account(self.name.as_ref())?
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;

        let uid = context
            .credentials
            .get_user_id(&service_account.user.system_name)
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;

        let gid = context
            .credentials
            .get_group_id(&service_account.group.system_name)
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;

        let mounts = context
            .seedling_definition
            .mounts
            .iter()
            .map(|(name, definition)| {
                let host_path = context
                    .douglas_folders
                    .seedling_mount(context.name.as_ref(), name.as_ref());

                Ok(MountDefinition {
                    name: name.as_ref().parse()?,
                    host_path,
                    container_path: definition.remote_path().to_path_buf(),
                    writable: *definition.access_mode() == seedbank_types::AccessMode::Writable,
                })
            })
            .collect::<Result<Vec<MountDefinition>, DockerNameError>>()?;

        let environment_variables = if context.seedling_definition.secrets.is_some() {
            vec![docker_types::EnvironmentVariable::new(
                "OPENBAO_AGENT_ADDR",
                &format!(
                    "http://{}:{}",
                    agent_container_name(&self.name)?.as_ref(),
                    openbao::AGENT_LOCAL_PROXY_PORT
                ),
            )]
        } else {
            Vec::new()
        };

        let new_container = build_new_container(
            new_container_name,
            context.seedling_definition,
            uid,
            gid,
            mounts,
            context.version,
            environment_variables,
        );

        context
            .docker_client
            .create_container(context.registry, &new_container)
            .await?;

        let seedling_network = seedling_network_name(&self.name)?;
        let private_subnet = context
            .agent_provisioning
            .map(|agent_provisioning| agent_provisioning.private_subnet.clone());
        if !context
            .docker_client
            .network_exists(&seedling_network)
            .await?
        {
            context
                .docker_client
                .create_network(&seedling_network, private_subnet.as_ref())
                .await?;
        }
        context
            .docker_client
            .connect_network(
                &seedling_network,
                ContainerRef::FullName(new_container.name.clone()),
                None,
            )
            .await?;

        if context.seedling_definition.origin == seedbank_types::Origin::Core {
            let system_network: docker_types::NetworkName = SYSTEM_NETWORK_NAME
                .parse()
                .expect("SYSTEM_NETWORK_NAME is a valid network name");
            context
                .docker_client
                .connect_network(
                    &system_network,
                    ContainerRef::FullName(new_container.name),
                    None,
                )
                .await?;
        }

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct BuildAgentContainer {
    seedling_name: seedbank_types::Name,
    version: seedbank_types::Version,
}

impl BuildAgentContainer {
    pub fn new(seedling_name: seedbank_types::Name, version: seedbank_types::Version) -> Self {
        Self {
            seedling_name,
            version,
        }
    }
}

impl std::fmt::Display for BuildAgentContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Building OpenBao agent container for '{}' (v{})",
            self.seedling_name, self.version
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for BuildAgentContainer {
    fn name(&self) -> String {
        "Building OpenBao agent container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Building OpenBao agent container for '{}' (v{})…",
                    self.seedling_name, self.version
                ),
                ScopeKind::Step,
            )
            .start_guard();

        let Some(agent_provisioning) = context.agent_provisioning else {
            guard.finish_with_outcome(log::Outcome::Failed);
            return Err(Box::new(ReconcileSeedlingError::MissingAgentProvisioning));
        };

        let new_container_name = agent_container_name(&self.seedling_name)?;
        let account_name = provision_seedling_secrets::agent_account_full_name(&self.seedling_name);
        let service_account = context
            .rolodex
            .find_service_account(&account_name)?
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;

        let uid = context
            .credentials
            .get_user_id(&service_account.user.system_name)
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;
        let gid = context
            .credentials
            .get_group_id(&service_account.group.system_name)
            .ok_or(ReconcileSeedlingError::MissingServiceAccount)?;

        let mount = MountDefinition {
            name: "config".parse()?,
            host_path: agent_provisioning.mount_dir.clone(),
            container_path: PathBuf::from(provision_seedling_secrets::AGENT_CONTAINER_MOUNT_PATH),
            writable: false,
        };

        let new_container = NewContainer {
            name: new_container_name,
            run_as: Some(ContainerUser {
                user_id: uid,
                group_id: gid,
            }),
            command: Some(format!(
                "agent -config={}/{}",
                provision_seedling_secrets::AGENT_CONTAINER_MOUNT_PATH,
                provision_seedling_secrets::AGENT_CONFIG_FILE_NAME
            )),
            environment_variables: Vec::new(),
            image: VersionedImageName::namespaced_specific(
                "openbao",
                "openbao",
                openbao::IMAGE_VERSION,
            ),
            mounts: vec![mount],
            added_capabilities: std::collections::HashSet::new(),
            labels: vec![
                labels::create_version_label(&self.version),
                labels::create_origin_label(context.seedling_definition.origin),
            ],
            published_ports: Vec::new(),
        };

        context
            .docker_client
            .create_container(context.registry, &new_container)
            .await?;

        let seedling_network = seedling_network_name(&self.seedling_name)?;
        if !context
            .docker_client
            .network_exists(&seedling_network)
            .await?
        {
            context
                .docker_client
                .create_network(&seedling_network, Some(&agent_provisioning.private_subnet))
                .await?;
        }
        context
            .docker_client
            .connect_network(
                &seedling_network,
                ContainerRef::FullName(new_container.name.clone()),
                Some(agent_provisioning.agent_private_ip),
            )
            .await?;

        let system_network: docker_types::NetworkName = SYSTEM_NETWORK_NAME
            .parse()
            .expect("SYSTEM_NETWORK_NAME is a valid network name");
        context
            .docker_client
            .connect_network(
                &system_network,
                ContainerRef::FullName(new_container.name),
                None,
            )
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct StartContainer {
    name: ContainerName,
    version: seedbank_types::Version,
}

impl StartContainer {
    pub fn new(name: ContainerName, version: seedbank_types::Version) -> Self {
        Self { name, version }
    }
}

impl std::fmt::Display for StartContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Starting container '{}' (v{})", self.name, self.version)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartContainer {
    fn name(&self) -> String {
        "Starting container".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Starting container '{}' (v{})…", self.name, self.version),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client
            .start_container(ContainerRef::FullName(self.name.clone()))
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seedbank_types::{HealthCheck, HealthCheckCommand};
    use std::{
        collections::{HashMap, HashSet},
        num::NonZeroU8,
        str::FromStr,
    };

    fn name() -> seedbank_types::Name {
        "traefik".parse().unwrap()
    }

    fn registry() -> docker_types::Registry {
        "localhost:7376".parse().unwrap()
    }

    fn seedling_definition() -> seedbank_types::SeedlingDefinition {
        seedbank_types::SeedlingDefinition::new(
            VersionedImageName::specific("traefik", "v3.7.7"),
            HashMap::new(),
            seedbank_types::Routing::None,
            HealthCheck {
                command: HealthCheckCommand::from_str("true").unwrap(),
                wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
            },
        )
    }

    fn state() -> State {
        State {
            seedling_version: VersionComparison::Current,
            container_name: container_name(&name()).unwrap(),
            image_status: ImageStatus::Local,
            container_status: VersionComparison::Current,
            container_version: Some(seedbank_types::Version(1)),
            container_is_running: true,
            missing_service_credentials: false,
            missing_share_groups: Vec::new(),
            missing_mount_folders: Vec::new(),
            needs_mount_ownership: Vec::new(),
            missing_mount_mode: Vec::new(),
            missing_mount_files: Vec::new(),
            agent_missing_service_credentials: false,
            agent_container_exists: true,
            agent_container_is_running: true,
        }
    }

    fn step_descriptions(steps: Vec<Step<Context<'_>>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_refuse_to_downgrade() {
        let result = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            State {
                container_status: VersionComparison::Newer,
                container_version: Some(seedbank_types::Version(2)),
                ..state()
            },
            &registry(),
        );

        assert!(matches!(
            result,
            Err(ReconcileSeedlingError::WouldDowngrade)
        ));
    }

    #[test]
    fn test_create_plan_should_error_when_the_image_is_unavailable() {
        let result = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            State {
                image_status: ImageStatus::Unknown,
                ..state()
            },
            &registry(),
        );

        assert!(matches!(result, Err(ReconcileSeedlingError::MissingImage)));
    }

    #[test]
    fn test_create_plan_should_do_nothing_when_everything_is_current() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            state(),
            &registry(),
        )
        .expect("should produce a plan");

        assert!(step_descriptions(steps).is_empty());
    }

    #[test]
    fn test_create_plan_should_create_a_missing_share_group() {
        let mount_name: seedbank_types::Name = "config".parse().unwrap();

        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            State {
                missing_share_groups: vec![(mount_name, vec!["openbao".to_string()])],
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Creating share group 'config' for 'traefik'"]
        );
    }

    #[test]
    fn test_create_plan_should_set_ownership_for_a_persisted_shared_mount() {
        let mount_name: seedbank_types::Name = "config".parse().unwrap();
        let path = PathBuf::from("/var/lib/douglas/mounts/traefik/config");

        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            State {
                needs_mount_ownership: vec![(
                    path.clone(),
                    mount_name,
                    seedbank_types::MountType::PersistedShared(vec![]),
                )],
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![format!(
                "Setting mount ownership '{}'",
                path_to_string(&path)
            )]
        );
    }

    #[test]
    fn test_create_plan_should_build_and_start_a_missing_container() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(2),
            &seedling_definition(),
            State {
                container_status: VersionComparison::Missing,
                container_version: None,
                container_is_running: false,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Building container 'traefik' (v2)",
                "Starting container 'doug.traefik' (v2)",
            ]
        );
    }

    fn secrets_seedling_definition() -> seedbank_types::SeedlingDefinition {
        seedling_definition().with_secrets_access(Some(seedbank_types::SecretsAccess {
            read: true,
            write: true,
        }))
    }

    #[test]
    fn test_create_plan_should_build_and_start_a_missing_agent_container() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &secrets_seedling_definition(),
            State {
                agent_container_exists: false,
                agent_container_is_running: false,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Ensuring OpenBao agent mount for 'traefik'",
                "Building OpenBao agent container for 'traefik' (v1)",
                "Starting container 'doug-agent.traefik' (v1)",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_only_start_an_existing_stopped_agent_container() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &secrets_seedling_definition(),
            State {
                agent_container_exists: true,
                agent_container_is_running: false,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Ensuring OpenBao agent mount for 'traefik'",
                "Starting container 'doug-agent.traefik' (v1)",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_create_the_agent_service_account_when_missing() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &secrets_seedling_definition(),
            State {
                agent_missing_service_credentials: true,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert!(
            step_descriptions(steps)
                .contains(&"Creating OpenBao agent service account for 'traefik'".to_string())
        );
    }

    #[test]
    fn test_create_plan_should_ignore_the_agent_entirely_when_secrets_are_not_requested() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &seedling_definition(),
            State {
                agent_container_exists: false,
                agent_container_is_running: false,
                agent_missing_service_credentials: true,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert!(step_descriptions(steps).is_empty());
    }

    #[test]
    fn test_create_plan_should_do_nothing_for_a_running_agent_container() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(1),
            &secrets_seedling_definition(),
            state(),
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Ensuring OpenBao agent mount for 'traefik'"]
        );
    }

    #[test]
    fn test_create_plan_should_stop_drop_rebuild_and_restart_a_stale_running_container() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(2),
            &seedling_definition(),
            State {
                container_status: VersionComparison::Stale,
                container_version: Some(seedbank_types::Version(1)),
                container_is_running: true,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Stopping container 'doug.traefik' (v1)",
                "Dropping container 'doug.traefik' (v1)",
                "Building container 'traefik' (v2)",
                "Starting container 'doug.traefik' (v2)",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_stopping_a_stale_container_that_is_not_running() {
        let steps = create_plan(
            &name(),
            &seedbank_types::Version(2),
            &seedling_definition(),
            State {
                container_status: VersionComparison::Stale,
                container_version: Some(seedbank_types::Version(1)),
                container_is_running: false,
                ..state()
            },
            &registry(),
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Dropping container 'doug.traefik' (v1)",
                "Building container 'traefik' (v2)",
                "Starting container 'doug.traefik' (v2)",
            ]
        );
    }

    #[test]
    fn test_published_ports_for_should_be_empty_when_no_ports_are_declared() {
        let definition = seedling_definition();

        let result = published_ports_for(&definition);

        assert_eq!(result, Vec::new());
    }

    #[test]
    fn test_published_ports_for_should_include_explicitly_published_ports() {
        let definition =
            seedling_definition().with_published_ports(vec![seedbank_types::PortMapping {
                external: 80,
                internal: 80,
            }]);

        let result = published_ports_for(&definition);

        assert_eq!(
            result,
            vec![docker_types::PortMapping {
                host_port: 80,
                container_port: 80,
            }]
        );
    }

    #[test]
    fn test_published_ports_for_should_include_a_routed_seedlings_additional_ports() {
        let mut definition = seedling_definition();
        definition.routing = seedbank_types::Routing::Routed {
            route: seedbank_types::RouteSpec::Root,
            ports: seedbank_types::PortSpec {
                public: 8080,
                additional: vec![seedbank_types::PortMapping {
                    external: 1234,
                    internal: 4321,
                }],
            },
        };

        let result = published_ports_for(&definition);

        assert_eq!(
            result,
            vec![docker_types::PortMapping {
                host_port: 1234,
                container_port: 4321,
            }]
        );
    }

    #[test]
    fn test_published_ports_for_should_merge_explicit_and_routed_additional_ports() {
        let mut definition =
            seedling_definition().with_published_ports(vec![seedbank_types::PortMapping {
                external: 80,
                internal: 80,
            }]);
        definition.routing = seedbank_types::Routing::Routed {
            route: seedbank_types::RouteSpec::Root,
            ports: seedbank_types::PortSpec {
                public: 8080,
                additional: vec![seedbank_types::PortMapping {
                    external: 1234,
                    internal: 4321,
                }],
            },
        };

        let result = published_ports_for(&definition);

        assert_eq!(
            result,
            vec![
                docker_types::PortMapping {
                    host_port: 80,
                    container_port: 80,
                },
                docker_types::PortMapping {
                    host_port: 1234,
                    container_port: 4321,
                },
            ]
        );
    }

    #[test]
    fn test_published_ports_for_should_not_publish_the_routed_public_port_by_itself() {
        let mut definition = seedling_definition();
        definition.routing = seedbank_types::Routing::Routed {
            route: seedbank_types::RouteSpec::Root,
            ports: seedbank_types::PortSpec {
                public: 8080,
                additional: Vec::new(),
            },
        };

        let result = published_ports_for(&definition);

        assert_eq!(result, Vec::new());
    }

    #[test]
    fn test_build_new_container_should_carry_the_seedling_definitions_command_and_capabilities() {
        let definition = seedling_definition()
            .with_command("server -config=/openbao/config/config.json")
            .with_capability(docker_types::Capability::Chown);

        let new_container = build_new_container(
            container_name(&name()).unwrap(),
            &definition,
            1000,
            1000,
            Vec::new(),
            &seedbank_types::Version(1),
            Vec::new(),
        );

        assert_eq!(
            new_container.command,
            Some("server -config=/openbao/config/config.json".to_string())
        );
        assert_eq!(
            new_container.added_capabilities,
            HashSet::from([docker_types::Capability::Chown])
        );
    }

    #[test]
    fn test_build_new_container_should_leave_command_unset_when_the_definition_has_none() {
        let definition = seedling_definition();

        let new_container = build_new_container(
            container_name(&name()).unwrap(),
            &definition,
            1000,
            1000,
            Vec::new(),
            &seedbank_types::Version(1),
            Vec::new(),
        );

        assert_eq!(new_container.command, None);
        assert!(new_container.added_capabilities.is_empty());
    }

    #[test]
    fn test_build_new_container_should_carry_the_provided_environment_variables() {
        let definition = seedling_definition();
        let environment_variables = vec![docker_types::EnvironmentVariable::new(
            "OPENBAO_AGENT_ADDR",
            "http://doug-agent.secrets:8100",
        )];

        let new_container = build_new_container(
            container_name(&name()).unwrap(),
            &definition,
            1000,
            1000,
            Vec::new(),
            &seedbank_types::Version(1),
            environment_variables.clone(),
        );

        assert_eq!(new_container.environment_variables, environment_variables);
    }

    #[test]
    fn test_build_new_container_should_label_the_container_with_the_definitions_origin() {
        let definition = seedling_definition().with_origin(seedbank_types::Origin::Core);

        let new_container = build_new_container(
            container_name(&name()).unwrap(),
            &definition,
            1000,
            1000,
            Vec::new(),
            &seedbank_types::Version(1),
            Vec::new(),
        );

        assert_eq!(
            labels::get_origin(&new_container.labels),
            Some(seedbank_types::Origin::Core)
        );
    }

    #[test]
    fn test_build_new_container_should_label_a_user_origin_seedling_accordingly() {
        let definition = seedling_definition().with_origin(seedbank_types::Origin::User);

        let new_container = build_new_container(
            container_name(&name()).unwrap(),
            &definition,
            1000,
            1000,
            Vec::new(),
            &seedbank_types::Version(1),
            Vec::new(),
        );

        assert_eq!(
            labels::get_origin(&new_container.labels),
            Some(seedbank_types::Origin::User)
        );
    }
}
