use crate::{
    blueprints::{EXPECTED_MOUNT_MODE, container_name, seedling_mount_path},
    labels::{self},
    rolodex::{Rolodex, RolodexError},
};
use async_trait::async_trait;
use blueprint::{
    Command,
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
    #[error("Invalid mount path {0}")]
    InvalidMountPath(RelativePath),
    #[error("Seedbank error {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Refusing to downgrade: the running container is newer than the declared version")]
    WouldDowngrade,
}

struct Context<'a> {
    name: &'a seedbank_types::Name,
    version: &'a seedbank_types::Version,
    seedling_definition: &'a seedbank_types::SeedlingDefinition,
    credentials: &'a dyn Credentials,
    docker_client: &'a mut dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
    douglas_folders: &'a DouglasFolders,
    rolodex: &'a dyn Rolodex,
    folder: &'a dyn Folder,
    file_writer: &'a dyn FileWriter,
    permissions: &'a dyn Permissions,
    registry: &'a docker_types::Registry,
    origin: labels::Origin,
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
    missing_mount_folders: Vec<PathBuf>,
    needs_mount_ownership: Vec<PathBuf>,
    missing_mount_mode: Vec<(PathBuf, Modes)>,
    missing_mount_files: Vec<(PathBuf, String, Vec<u8>)>,
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
    origin: labels::Origin,
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
            .discover(guard.span(), name, version, seedling_definition)
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
            credentials,
            docker_client: &mut *docker_client,
            seedbank_client,
            rolodex,
            douglas_folders,
            folder,
            permissions,
            file_writer,
            registry,
            origin,
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
            missing_mount_folders: Vec::new(),
            needs_mount_ownership: Vec::new(),
            missing_mount_mode: Vec::new(),
            missing_mount_files: Vec::new(),
        };

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
        let root_mount_path = self.mount_path(seedling_name, name);
        if self.folder.exists(&root_mount_path) {
            match self.expected_mount_group_name(seedling_name, name, mount)? {
                Some(expected_group) => {
                    let (_actual_user, actual_group) = self
                        .permissions
                        .get_user_and_group_ownership(&root_mount_path)?;
                    if actual_group != expected_group {
                        result.needs_mount_ownership.push(root_mount_path.clone());
                    }
                }
                None => {
                    result.needs_mount_ownership.push(root_mount_path.clone());
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
            self.add_missing_mount(result, mount, &root_mount_path)?;
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
        seedling_mount_path(self.douglas_folders, seedling_name, name)
    }

    fn add_missing_mount(
        &self,
        result: &mut State,
        mount: &seedbank_types::Mount,
        root_mount_path: &Path,
    ) -> Result<(), ReconcileSeedlingError> {
        result
            .missing_mount_folders
            .push(root_mount_path.to_path_buf());
        result
            .needs_mount_ownership
            .push(root_mount_path.to_path_buf());
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

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn create_plan<'a>(
    name: &seedbank_types::Name,
    version: &seedbank_types::Version,
    seedling_definition: &seedbank_types::SeedlingDefinition,
    mut state: State,
    registry: &docker_types::Registry,
) -> Result<Vec<Step<'a>>, ReconcileSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

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

    for missing_mount_folder in state.missing_mount_folders {
        push_step(&mut steps, CreateMountFolder::new(missing_mount_folder));
    }

    for path in state.needs_mount_ownership {
        push_step(&mut steps, SetMountOwnership::new(path));
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

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
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
}

impl SetMountOwnership {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
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

        context.permissions.change_user_and_group_ownership(
            &self.path,
            &service_account.user.system_name,
            &service_account.group.system_name,
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
                let host_path = seedling_mount_path(context.douglas_folders, context.name, name);

                Ok(MountDefinition {
                    name: name.as_ref().parse()?,
                    host_path,
                    container_path: definition.remote_path().to_path_buf(),
                    writable: *definition.access_mode() == seedbank_types::AccessMode::Writable,
                })
            })
            .collect::<Result<Vec<MountDefinition>, DockerNameError>>()?;

        let new_container = NewContainer {
            name: new_container_name,
            run_as: Some(ContainerUser {
                user_id: uid,
                group_id: gid,
            }),
            command: None,
            environment_variables: Vec::new(),
            image: context.seedling_definition.image.clone(),
            mounts,
            added_capabilities: Vec::new(),
            labels: vec![
                labels::create_version_label(context.version),
                labels::create_origin_label(context.origin),
            ],
        };

        context
            .docker_client
            .create_container(context.registry, &new_container)
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
    use std::collections::HashMap;

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
            missing_mount_folders: Vec::new(),
            needs_mount_ownership: Vec::new(),
            missing_mount_mode: Vec::new(),
            missing_mount_files: Vec::new(),
        }
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
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
}
