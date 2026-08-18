mod bootstrap;
mod protocol;
mod registration_protocol;

pub use bootstrap::{DOUGLAS_SEEDBANK_GROUP, DOUGLAS_SEEDBANK_USER, service_definition};
pub use protocol::handle;
pub use seedbank_types::{
    Id, IdParseError, MAX_SEEDLINGS, Mount, MountContents, MountFile, MountType, Name,
    NameParseError, NameRules, Request, Response, Seedling, SeedlingDefinition,
};
use seedbank_types::{SeedlingStatus, Version};

use blueprint::listener::SocketListenerFactory;
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    BindableUnixDomainSocketFile, Entry, EntryKind, FileDeleter, FileReader, FileSystemError,
    FileWriter, Folder, FolderDeleter, Inspect, Permissions, RelativePath, UnixDomainSocket,
    UnixFileDeleter, UnixFileReader, UnixFileWriter, UnixFolder, UnixFolderDeleter, UnixInspect,
    UnixPermissions,
};
use log::{BufferedFileReporter, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokio::sync::broadcast::{self, Sender};

pub(crate) static SEEDS_ROOT_NAME: &str = "seeds";
pub static SEEDBANK: &str = "seedbank";

#[derive(Error, Debug)]
pub enum Error {
    #[error("Cannot be root")]
    CannotBeRoot,
    #[error("Missing be root path")]
    MissingRootPath,
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Seedling already exists {0}")]
    AlreadyExists(Name),
    #[error("Seedling does not exist {0}")]
    NotFound(Name),
    #[error("Updating version must be greater than the current version")]
    InvalidVersion,
    #[error("Seedling limit reached")]
    TooManySeedlings,
    #[error("Default is already claimed by {0}")]
    DefaultAlreadyClaimed(Name),

    #[error("IO Error {0}")]
    IoError(#[from] std::io::Error),
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Name parse error: {0}")]
    NameParseError(#[from] NameParseError),
    #[error("Id parse error: {0}")]
    IdParseError(#[from] IdParseError),
    #[error("Version parse error: {0}")]
    VersionParseError(#[from] std::num::ParseIntError),
    #[error("Seedling serialization error: {0}")]
    SeedlingSerializationError(#[from] toml::ser::Error),
    #[error("Seedling deserialization error: {0}")]
    SeedlingDeserializationError(#[from] toml::de::Error),
}

#[cfg_attr(test, mockall::automock)]
pub trait Seedbank {
    fn list(&self) -> Result<Vec<Name>, Error>;
    fn exists(&self, name: &Name) -> Result<bool, Error>;
    fn status(&self, name: &Name) -> Result<SeedlingStatus, Error>;
    fn load(&self, name: &Name) -> Result<Seedling, Error>;
    fn create(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error>;
    fn delete(&self, name: &Name) -> Result<(), Error>;
    fn update(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error>;
    fn default_seedling(&self) -> Result<Option<Name>, Error>;
    fn claim_default(&self, name: &Name) -> Result<(), Error>;
    fn release_default(&self, name: &Name) -> Result<(), Error>;
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    folder: Arc<dyn Folder>,
    folder_deleter: Arc<dyn FolderDeleter>,
    file_deleter: Arc<dyn FileDeleter>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    inspect: Arc<dyn Inspect>,
    seeds: PathBuf,
    listener_factory: SocketListenerFactory,
    registration_listener_factory: SocketListenerFactory,
    shutdown_sender: Sender<()>,
    global_lock: Mutex<()>,
}

impl Server {
    pub async fn build(reporting_fd: Option<i32>) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
        let folder_deleter: Arc<dyn FolderDeleter> = Arc::new(UnixFolderDeleter::new());
        let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
        let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let inspect: Arc<dyn Inspect> = Arc::new(UnixInspect::new());
        let unix_domain_socket: Arc<dyn BindableUnixDomainSocketFile> =
            Arc::new(UnixDomainSocket::new());
        let douglas_folders = DouglasFolders::new();
        let permissions: Arc<dyn Permissions> = Arc::new(UnixPermissions::new());
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        let reporter: Arc<dyn Reporter> = if reporting_fd.is_none() {
            Arc::new(TuiReporter::start()?)
        } else {
            bootstrap::bootstrap(
                reporting_fd,
                &*credentials,
                &*folder,
                &*permissions,
                &douglas_folders,
            )
            .await?;
            Arc::new(BufferedFileReporter::new(
                douglas_folders.service_log_file(SEEDBANK),
            ))
        };

        let mut seeds = douglas_folders.service_root(SEEDBANK);
        seeds.push(SEEDS_ROOT_NAME);

        let mut owned_sockets = service_definition(&douglas_folders)
            .owned_sockets
            .into_iter();
        let listener_factory = SocketListenerFactory::new(
            owned_sockets
                .next()
                .expect("seedbank always defines its main control socket"),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&unix_domain_socket),
        );
        let registration_listener_factory = SocketListenerFactory::new(
            owned_sockets
                .next()
                .expect("seedbank always defines its registration socket"),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&unix_domain_socket),
        );

        Ok(Self {
            reporter,
            folder,
            folder_deleter,
            file_deleter,
            file_reader,
            file_writer,
            inspect,
            seeds,
            listener_factory,
            registration_listener_factory,
            shutdown_sender,
            global_lock: Mutex::new(()),
        })
    }

    fn create_seedling_path(&self, name: &Name) -> PathBuf {
        let mut expected_path = self.seeds.clone();
        expected_path.push(name.to_string());
        expected_path
    }

    fn definition_path(&self, name: &Name) -> PathBuf {
        let mut path = self.create_seedling_path(name);
        path.push("seedling.toml");
        path
    }

    fn default_path(&self) -> PathBuf {
        self.seeds.join("default")
    }

    fn read_default(&self) -> Result<Option<Name>, Error> {
        match self.file_reader.read_all(&self.default_path()) {
            Ok(raw) => Ok(Some(Name::from_str(raw.trim())?)),
            Err(FileSystemError::IoErrorAtPath { error, .. })
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn id_path(&self, name: &Name) -> PathBuf {
        let mut path = self.create_seedling_path(name);
        path.push("id");
        path
    }

    fn version_path(&self, name: &Name) -> PathBuf {
        let mut path = self.create_seedling_path(name);
        path.push("version");
        path
    }

    fn mounts_root(&self, seedling_name: &Name) -> PathBuf {
        let mut path = self.create_seedling_path(seedling_name);
        path.push("mounts");
        path
    }

    fn mount_root(&self, seedling_name: &Name, mount_name: &Name) -> PathBuf {
        let mut path = self.mounts_root(seedling_name);
        path.push(mount_name.to_string());
        path
    }

    fn mount_file(&self, seedling_name: &Name, mount_name: &Name, file: &RelativePath) -> PathBuf {
        let path = self.mount_root(seedling_name, mount_name);
        path.join(file)
    }

    fn create_mount_lookup(
        seedling_definition: Option<&SeedlingDefinition>,
    ) -> HashMap<Name, HashSet<MountContents>> {
        seedling_definition
            .iter()
            .flat_map(|definitions| &definitions.mounts)
            .map(|(name, mount)| (name.clone(), mount.contents().clone()))
            .collect()
    }

    fn write_definition(
        &self,
        seedling_name: &Name,
        version: &Version,
        current: Option<&SeedlingDefinition>,
        proposed: &SeedlingDefinition,
    ) -> Result<(), Error> {
        self.purge_mounts(seedling_name, current, proposed)?;

        let contents = toml::to_string(proposed)?;
        self.write_mounts(seedling_name, &proposed.mounts)?;
        self.file_writer
            .write_all(&self.definition_path(seedling_name), &contents)?;
        self.write_version(seedling_name, version)?;
        Ok(())
    }

    fn purge_mounts(
        &self,
        name: &Name,
        current: Option<&SeedlingDefinition>,
        proposed: &SeedlingDefinition,
    ) -> Result<(), FileSystemError> {
        let proposed_mount_lookup = Self::create_mount_lookup(Some(proposed));
        let current_mount_lookup = Self::create_mount_lookup(current);

        for (mount_name, current_contents) in current_mount_lookup {
            match proposed_mount_lookup.get(&mount_name) {
                Some(proposed_contents) => {
                    for stale in current_contents.difference(proposed_contents) {
                        self.delete_mount_contents(name, &mount_name, stale)?;
                    }
                }
                None => {
                    self.folder_deleter
                        .delete(&self.mount_root(name, &mount_name))?;
                }
            };
        }
        Ok(())
    }

    fn delete_mount_contents(
        &self,
        seedling_name: &Name,
        mount_name: &Name,
        contents: &MountContents,
    ) -> Result<(), FileSystemError> {
        let path = self.mount_file(seedling_name, mount_name, contents.relative_path());

        match contents {
            MountContents::File(_) => self.file_deleter.delete(&path),
            MountContents::FolderOnly(_) => self.folder_deleter.delete(&path),
        }
    }

    fn write_id(&self, name: &Name, id: &Id) -> Result<(), Error> {
        self.file_writer
            .write_all(&self.id_path(name), &id.to_string())?;
        Ok(())
    }

    fn write_version(&self, name: &Name, version: &Version) -> Result<(), Error> {
        self.file_writer
            .write_all(&self.version_path(name), &version.to_string())?;
        Ok(())
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Error> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting seedbank",
            ScopeKind::Group,
        );

        let mut shutdown = self.shutdown_sender.subscribe();

        let listener = self.listener_factory.create(&span)?;
        let registration_listener = self.registration_listener_factory.create(&span)?;

        let accept_loops = async {
            let main_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::accept_loop(listener, server).await })
            };
            let registration_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move {
                    Self::accept_registration_loop(registration_listener, server).await
                })
            };

            main_task.await.map_err(std::io::Error::other)??;
            registration_task.await.map_err(std::io::Error::other)??;
            Ok::<_, Error>(())
        };

        tokio::select! {
            r = accept_loops => r?,
            _ = shutdown.recv() => {},
        }

        span.create_scoped_reporter().finish(log::Outcome::Ok);
        Ok(())
    }

    async fn accept_loop(
        listener: Box<dyn file_system::Listener + Send + Sync + 'static>,
        server: Arc<Self>,
    ) -> Result<(), Error> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                Self::handle_connection(stream, server).await;
            });
        }
    }

    async fn handle_connection(mut stream: tokio::net::UnixStream, server: Arc<Self>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();

        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => protocol::handle(server.as_ref(), request),
            Err(err) => Response::Error {
                message: err.to_string(),
            },
        };

        let serialized = match serde_json::to_string(&response) {
            Ok(serialized) => serialized,
            Err(err) => {
                Self::log_connection_error(&server, "Failed to serialize response", &err);
                return;
            }
        };

        if let Err(err) = writer.write_all(format!("{serialized}\n").as_bytes()).await {
            Self::log_connection_error(&server, "Failed to write response", &err);
        }
    }

    fn log_connection_error(server: &Arc<Self>, label: &str, err: &impl std::fmt::Display) {
        Span::new(
            Arc::clone(&server.reporter),
            "Handling connection",
            ScopeKind::Task,
        )
        .message(log::Level::Warn, &format!("{label}: {err}"));
    }

    async fn accept_registration_loop(
        listener: Box<dyn file_system::Listener + Send + Sync + 'static>,
        server: Arc<Self>,
    ) -> Result<(), Error> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                Self::handle_registration_connection(stream, server).await;
            });
        }
    }

    async fn handle_registration_connection(mut stream: tokio::net::UnixStream, server: Arc<Self>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();

        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };

        let Ok(request) = serde_json::from_str::<seedling_registration_types::Request>(&line)
        else {
            return;
        };

        let response = match registration_protocol::handle(server.as_ref(), request) {
            Ok(response) => response,
            Err(err) => {
                Self::log_connection_error(&server, "Failed to handle registration request", &err);
                return;
            }
        };

        let serialized = match serde_json::to_string(&response) {
            Ok(serialized) => serialized,
            Err(err) => {
                Self::log_connection_error(&server, "Failed to serialize response", &err);
                return;
            }
        };

        if let Err(err) = writer.write_all(format!("{serialized}\n").as_bytes()).await {
            Self::log_connection_error(&server, "Failed to write response", &err);
        }
    }

    fn list_seedlings(&self) -> Result<Vec<Name>, Error> {
        Ok(self
            .folder
            .entries(&self.seeds)?
            .iter()
            .filter(|entry| entry.kind == EntryKind::Directory)
            .filter_map(|entry| Name::from_str(&entry.name).ok())
            .collect())
    }

    fn seedling_exists(&self, name: &Name) -> Result<bool, Error> {
        let expected_path = self.create_seedling_path(name);
        Ok(self.folder.exists(&expected_path))
    }

    fn seedling_status(&self, name: &Name) -> Result<SeedlingStatus, Error> {
        let expected_path = self.create_seedling_path(name);
        if self.folder.exists(&expected_path) {
            Ok(SeedlingStatus::Defined(self.load_version(name)?))
        } else {
            Ok(SeedlingStatus::Unknown)
        }
    }

    fn get_next_id(&self) -> Result<Id, Error> {
        let used_ids: HashSet<u16> = self
            .list_seedlings()?
            .iter()
            .map(|name| match self.load_id(name) {
                Ok(id) => Ok(Some(id.value)),
                Err(Error::NotFound(_)) => Ok(None),
                Err(err) => Err(err),
            })
            .collect::<Result<Vec<Option<u16>>, Error>>()?
            .into_iter()
            .flatten()
            .collect();

        (0..MAX_SEEDLINGS)
            .find(|id| !used_ids.contains(id))
            .map(|value| Id { value })
            .ok_or(Error::TooManySeedlings)
    }

    fn load_id(&self, name: &Name) -> Result<Id, Error> {
        match self.file_reader.read_all(&self.id_path(name)) {
            Ok(raw) => Ok(Id::from_str(raw.trim())?),
            Err(FileSystemError::IoErrorAtPath { error, .. })
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(Error::NotFound(name.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn load_version(&self, name: &Name) -> Result<Version, Error> {
        match self.file_reader.read_all(&self.version_path(name)) {
            Ok(raw) => Ok(Version::from_str(raw.trim())?),
            Err(FileSystemError::IoErrorAtPath { error, .. })
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(Error::NotFound(name.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn load_seedling(&self, name: &Name) -> Result<Seedling, Error> {
        let definition = self.load_definition(name)?;
        let id = self.load_id(name)?;
        let version = self.load_version(name)?;

        Ok(Seedling {
            id,
            name: name.clone(),
            definition,
            version,
        })
    }

    fn load_definition(&self, seedling_name: &Name) -> Result<SeedlingDefinition, Error> {
        match self
            .file_reader
            .read_all(&self.definition_path(seedling_name))
        {
            Ok(raw) => {
                let mut result = toml::from_str::<SeedlingDefinition>(&raw)?;
                self.hydrate_mounts(seedling_name, &mut result)?;
                Ok(result)
            }
            Err(FileSystemError::IoErrorAtPath { error, .. })
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(Error::NotFound(seedling_name.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn hydrate_mounts(
        &self,
        seedling_name: &Name,
        seedling_definition: &mut SeedlingDefinition,
    ) -> Result<(), Error> {
        for (mount_name, mount) in seedling_definition.mounts.iter_mut() {
            self.hydrate_mount_contents(seedling_name, mount_name, mount)?;
        }
        Ok(())
    }

    fn hydrate_mount_contents(
        &self,
        seedling_name: &Name,
        mount_name: &Name,
        mount: &mut Mount,
    ) -> Result<(), Error> {
        let mount_root = self.mount_root(seedling_name, mount_name);
        let root_entry = self.inspect.read_metadata(&mount_root)?;

        self.populate_mount_contents(&mount_root, root_entry, mount.contents_mut())
    }

    fn populate_mount_contents(
        &self,
        mount_root: &Path,
        entry: Entry,
        mount_contents: &mut HashSet<MountContents>,
    ) -> Result<(), Error> {
        if entry.is_link {
            return Err(FileSystemError::InvalidPath(entry.path).into());
        }

        match entry.kind {
            file_system::EntryKind::File => {
                mount_contents.insert(MountContents::File(MountFile {
                    file_relative_path: self
                        .folder
                        .create_relative_path(mount_root, &entry.path)?,
                    contents: self.file_reader.read_all_bytes(&entry.path)?,
                }));
                Ok(())
            }
            file_system::EntryKind::Directory => {
                let entries = self.folder.entries(&entry.path)?;
                if entries.is_empty() {
                    mount_contents.insert(MountContents::FolderOnly(
                        self.folder.create_relative_path(mount_root, &entry.path)?,
                    ));
                } else {
                    for entry in entries {
                        self.populate_mount_contents(mount_root, entry, mount_contents)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn write_mounts(
        &self,
        seedling_name: &Name,
        mounts: &HashMap<Name, Mount>,
    ) -> Result<(), FileSystemError> {
        for (mount_name, mount) in mounts {
            let root = self.mount_root(seedling_name, mount_name);
            self.folder.create_recursively(&root)?;

            for contents in mount.contents() {
                match contents {
                    MountContents::FolderOnly(relative_path) => {
                        let path = root.join(relative_path);
                        self.folder.create_recursively(&path)?;
                    }
                    MountContents::File(mount_file) => {
                        let mount_file_path = root.join(&mount_file.file_relative_path);
                        let Some(mount_path) = self.folder.parent(&mount_file_path) else {
                            return Err(FileSystemError::ExpectedFileError);
                        };
                        self.folder.create_recursively(&mount_path)?;

                        self.file_writer
                            .write_all_bytes(&mount_file_path, &mount_file.contents)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Seedbank for Server {
    fn list(&self) -> Result<Vec<Name>, Error> {
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");
        self.list_seedlings()
    }

    fn exists(&self, name: &Name) -> Result<bool, Error> {
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");
        self.seedling_exists(name)
    }

    fn status(&self, name: &Name) -> Result<SeedlingStatus, Error> {
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");
        self.seedling_status(name)
    }

    fn create(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Creating seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        if self.seedling_exists(name)? {
            return guard.finish(Err(Error::AlreadyExists(name.clone())));
        }

        let id = match self.get_next_id() {
            Ok(id) => id,
            Err(err) => return guard.finish(Err(err)),
        };

        let path = self.create_seedling_path(name);
        if let Err(err) = self.folder.create_recursively(&path) {
            return guard.finish(Err(err.into()));
        }

        if let Err(err) = self.write_id(name, &id) {
            return guard.finish(Err(err));
        }

        guard.finish(self.write_definition(name, version, None, definition))
    }

    fn load(&self, name: &Name) -> Result<Seedling, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Loading seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        guard.finish(self.load_seedling(name))
    }

    fn delete(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Deleting seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        if !self.seedling_exists(name)? {
            return guard.finish(Err(Error::NotFound(name.clone())));
        }
        let path = self.create_seedling_path(name);
        self.folder_deleter.delete(&path)?;

        guard.finish(Ok(()))
    }

    fn update(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Updating seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        match self.seedling_status(name)? {
            SeedlingStatus::Defined(current_version) => {
                if *version > current_version {
                    let current = match self.load_definition(name) {
                        Ok(current) => current,
                        Err(err) => return guard.finish(Err(err)),
                    };
                    guard.finish(self.write_definition(name, version, Some(&current), definition))
                } else {
                    guard.finish(Err(Error::InvalidVersion))
                }
            }
            SeedlingStatus::Unknown => guard.finish(Err(Error::NotFound(name.clone()))),
        }
    }

    fn default_seedling(&self) -> Result<Option<Name>, Error> {
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");
        self.read_default()
    }

    fn claim_default(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Claiming default for {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        if !self.seedling_exists(name)? {
            return guard.finish(Err(Error::NotFound(name.clone())));
        }

        if let Some(current) = match self.read_default() {
            Ok(current) => current,
            Err(err) => return guard.finish(Err(err)),
        } {
            if current != *name {
                return guard.finish(Err(Error::DefaultAlreadyClaimed(current)));
            }
            return guard.finish(Ok(()));
        }

        guard.finish(
            self.file_writer
                .write_all(&self.default_path(), name.as_ref())
                .map_err(Error::from),
        )
    }

    fn release_default(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Releasing default for {name}…"),
            ScopeKind::Task,
        )
        .start_guard();
        let _lock = self.global_lock.lock().expect("seedbank lock poisoned");

        match self.read_default() {
            Ok(Some(current)) if current == *name => guard.finish(
                self.file_deleter
                    .delete(&self.default_path())
                    .map_err(Error::from),
            ),
            Ok(_) => guard.finish(Ok(())),
            Err(err) => guard.finish(Err(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blueprint::listener::ListenerDefinition;
    use docker_types::VersionedImageName;
    use file_system::{
        Entry, MockBindableUnixDomainSocketFile, MockFileDeleter, MockFileReader, MockFileWriter,
        MockFolder, MockFolderDeleter, MockInspect, MockPermissions, Modes,
    };
    use log::Event;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("valid name")
    }

    fn id(value: u16) -> Id {
        Id { value }
    }

    fn version(value: u16) -> Version {
        Version::from_str(&value.to_string()).expect("valid version")
    }

    fn definition() -> SeedlingDefinition {
        SeedlingDefinition::new(
            VersionedImageName::latest("test"),
            HashMap::new(),
            seedbank_types::Routing::None,
        )
    }

    fn definition_with_mounts(mounts: HashMap<Name, Mount>) -> SeedlingDefinition {
        SeedlingDefinition::new(
            VersionedImageName::latest("test"),
            mounts,
            seedbank_types::Routing::None,
        )
    }

    fn mounts(pairs: Vec<(&str, Mount)>) -> HashMap<Name, Mount> {
        pairs.into_iter().map(|(n, m)| (name(n), m)).collect()
    }

    fn mount(contents: HashSet<MountContents>) -> Mount {
        Mount::build(
            MountType::Persisted,
            PathBuf::from("/etc/traefik"),
            seedbank_types::AccessMode::Writable,
            contents,
        )
    }

    fn dummy_listener_factory(socket_name: &str) -> SocketListenerFactory {
        SocketListenerFactory::new(
            ListenerDefinition::new(
                &PathBuf::from(format!("/tmp/{socket_name}.sock")),
                "douglas-seedbank",
                "douglas-seedbank",
                Modes::OwnerReadWriteGroupReadWrite,
            ),
            Arc::new(MockFileDeleter::new()),
            Arc::new(MockPermissions::new()),
            Arc::new(MockBindableUnixDomainSocketFile::new()),
        )
    }

    fn build_server(
        folder: MockFolder,
        folder_deleter: MockFolderDeleter,
        file_reader: MockFileReader,
        file_writer: MockFileWriter,
    ) -> Server {
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        Server {
            reporter: Arc::new(NullReporter),
            folder: Arc::new(folder),
            folder_deleter: Arc::new(folder_deleter),
            file_deleter: Arc::new(MockFileDeleter::new()),
            file_reader: Arc::new(file_reader),
            file_writer: Arc::new(file_writer),
            inspect: Arc::new(MockInspect::new()),
            seeds: PathBuf::from("/var/lib/seedbank/seeds"),
            listener_factory: dummy_listener_factory("seedbank"),
            registration_listener_factory: dummy_listener_factory("seedbank-registration"),
            shutdown_sender,
            global_lock: std::sync::Mutex::new(()),
        }
    }

    #[test]
    fn test_list_should_return_only_valid_names() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            "/var/lib/seedbank/seeds",
            vec![
                Entry::create_directory("valid-name"),
                Entry::create_directory("Invalid Name!"),
            ],
        );

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let names = server.list().expect("should list");

        assert_eq!(names, vec![name("valid-name")]);
    }

    #[test]
    fn test_exists_should_be_true_when_folder_present() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        assert!(server.exists(&name("foo")).expect("should check"));
    }

    #[test]
    fn test_exists_should_be_false_when_folder_missing() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        assert!(!server.exists(&name("foo")).expect("should check"));
    }

    #[test]
    fn test_status_should_return_defined_with_version_when_present() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/version", "3");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let status = server.status(&name("foo")).expect("should check status");

        assert_eq!(status, SeedlingStatus::Defined(version(3)));
    }

    #[test]
    fn test_status_should_return_unknown_when_missing() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let status = server.status(&name("foo")).expect("should check status");

        assert_eq!(status, SeedlingStatus::Unknown);
    }

    #[test]
    fn test_create_should_assign_next_id_and_write_seedling_manifest() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");
        folder.given_folder_entries("/var/lib/seedbank/seeds", Vec::new());
        folder.expect_create_folder_recursively_with("/var/lib/seedbank/seeds/foo");

        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_to_file_with_contents(
                "/var/lib/seedbank/seeds/foo/seedling.toml",
                &toml::to_string(&definition()).expect("should serialize"),
            )
            .expect_write_to_file_with_contents("/var/lib/seedbank/seeds/foo/id", "0")
            .expect_write_to_file_with_contents("/var/lib/seedbank/seeds/foo/version", "0");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            file_writer,
        );

        assert!(
            server
                .create(&name("foo"), &version(0), &definition())
                .is_ok()
        );
    }

    #[test]
    fn test_create_should_fail_when_already_exists() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let result = server.create(&name("foo"), &version(0), &definition());

        assert!(matches!(result, Err(Error::AlreadyExists(_))));
    }

    #[test]
    fn test_load_should_parse_seedling_manifest() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .given_can_read_all_with_contents(
                "/var/lib/seedbank/seeds/foo/seedling.toml",
                &toml::to_string(&definition()).expect("should serialize"),
            )
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/id", "0")
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/version", "0");

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let seedling = server.load(&name("foo")).expect("should load");

        assert_eq!(seedling.name, name("foo"));
        assert_eq!(seedling.id, id(0));
        assert_eq!(seedling.version, version(0));
    }

    #[test]
    fn test_load_should_fail_when_missing() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.load(&name("foo"));

        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    fn test_delete_should_remove_seedling_folder() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter.expect_folder_to_be_deleted("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            folder_deleter,
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        assert!(server.delete(&name("foo")).is_ok());
    }

    #[test]
    fn test_delete_should_fail_when_missing() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let result = server.delete(&name("foo"));

        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    fn test_update_should_overwrite_seedling_manifest() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader
            .given_can_read_all_with_contents(
                "/var/lib/seedbank/seeds/foo/seedling.toml",
                &toml::to_string(&definition()).expect("should serialize"),
            )
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/version", "0");

        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_to_file_with_contents(
                "/var/lib/seedbank/seeds/foo/seedling.toml",
                &toml::to_string(&definition()).expect("should serialize"),
            )
            .expect_write_to_file_with_contents("/var/lib/seedbank/seeds/foo/version", "1");

        let server = build_server(folder, MockFolderDeleter::new(), file_reader, file_writer);

        assert!(
            server
                .update(&name("foo"), &version(1), &definition())
                .is_ok()
        );
    }

    #[test]
    fn test_update_should_fail_when_missing() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let result = server.update(&name("foo"), &version(1), &definition());

        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    fn test_update_should_fail_when_version_is_not_newer() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/version", "1");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.update(&name("foo"), &version(1), &definition());

        assert!(matches!(result, Err(Error::InvalidVersion)));
    }

    #[test]
    fn test_update_should_fail_when_version_is_older() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/foo/version", "2");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.update(&name("foo"), &version(1), &definition());

        assert!(matches!(result, Err(Error::InvalidVersion)));
    }

    #[test]
    fn test_get_next_id_should_return_zero_when_no_seedlings_exist() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries("/var/lib/seedbank/seeds", Vec::new());

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let next = server.get_next_id().expect("should allocate");

        assert_eq!(next, id(0));
    }

    #[test]
    fn test_get_next_id_should_fill_the_first_gap_rather_than_extend() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            "/var/lib/seedbank/seeds",
            vec![
                Entry::create_directory("a"),
                Entry::create_directory("b"),
                Entry::create_directory("c"),
            ],
        );

        let mut file_reader = MockFileReader::new();
        file_reader
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/a/id", "0")
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/b/id", "2")
            .given_can_read_all_with_contents("/var/lib/seedbank/seeds/c/id", "3");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let next = server.get_next_id().expect("should allocate");

        assert_eq!(next, id(1));
    }

    #[test]
    fn test_get_next_id_should_skip_seedlings_with_no_assigned_id_yet() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            "/var/lib/seedbank/seeds",
            vec![Entry::create_directory("unassigned")],
        );

        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let next = server.get_next_id().expect("should allocate");

        assert_eq!(next, id(0));
    }

    #[test]
    fn test_get_next_id_should_propagate_a_genuine_id_read_error_rather_than_skip_it() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            "/var/lib/seedbank/seeds",
            vec![Entry::create_directory("corrupted")],
        );

        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })
        });

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.get_next_id();

        assert!(result.is_err());
    }

    #[test]
    fn test_get_next_id_should_error_when_every_slot_is_taken() {
        let entries: Vec<Entry> = (0..MAX_SEEDLINGS)
            .map(|value| Entry::create_directory(&format!("seed{value}")))
            .collect();

        let mut folder = MockFolder::new();
        folder.given_folder_entries("/var/lib/seedbank/seeds", entries);

        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            let Some(seedling_name) = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
            else {
                panic!("unexpected path {path:?}");
            };
            let value: u16 = seedling_name
                .trim_start_matches("seed")
                .parse()
                .expect("should be a synthetic seed name");

            Ok(value.to_string())
        });

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.get_next_id();

        assert!(matches!(result, Err(Error::TooManySeedlings)));
    }

    #[test]
    fn test_hydrate_mount_contents_should_populate_a_single_file() {
        let mount_root_path = "/var/lib/seedbank/seeds/foo/mounts/config";
        let file_path = "/var/lib/seedbank/seeds/foo/mounts/config/static.yml";

        let mut inspect = MockInspect::new();
        inspect.expect_entry_with_metadata(
            mount_root_path,
            Entry::create_directory_in("/var/lib/seedbank/seeds/foo/mounts", "config"),
        );

        let mut folder = MockFolder::new();
        folder
            .given_folder_entries(
                mount_root_path,
                vec![Entry::create_file_entry_in(mount_root_path, "static.yml")],
            )
            .given_relative_path_with(mount_root_path, file_path, "static.yml");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_bytes_with_contents(file_path, b"abc");

        let mut server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );
        server.inspect = Arc::new(inspect);

        let mut mount = mount(HashSet::new());
        server
            .hydrate_mount_contents(&name("foo"), &name("config"), &mut mount)
            .expect("should hydrate");

        let expected = MountContents::file("static.yml", b"abc").expect("valid content");
        assert_eq!(mount.contents(), &HashSet::from([expected]));
    }

    #[test]
    fn test_hydrate_mount_contents_should_record_an_empty_directory_as_folder_only() {
        let mount_root_path = "/var/lib/seedbank/seeds/foo/mounts/config";

        let mut inspect = MockInspect::new();
        inspect.expect_entry_with_metadata(
            mount_root_path,
            Entry::create_directory_in("/var/lib/seedbank/seeds/foo/mounts", "config"),
        );

        let mut folder = MockFolder::new();
        folder
            .given_folder_entries(mount_root_path, Vec::new())
            .given_relative_path_with(mount_root_path, mount_root_path, "");

        let mut server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.inspect = Arc::new(inspect);

        let mut mount = mount(HashSet::new());
        server
            .hydrate_mount_contents(&name("foo"), &name("config"), &mut mount)
            .expect("should hydrate");

        let expected = MountContents::folder_only("").expect("valid path");
        assert_eq!(mount.contents(), &HashSet::from([expected]));
    }

    #[test]
    fn test_hydrate_mount_contents_should_recurse_into_subdirectories() {
        let mount_root_path = "/var/lib/seedbank/seeds/foo/mounts/config";
        let subdir_path = "/var/lib/seedbank/seeds/foo/mounts/config/dynamic";
        let file_path = "/var/lib/seedbank/seeds/foo/mounts/config/dynamic/app.yml";

        let mut inspect = MockInspect::new();
        inspect.expect_entry_with_metadata(
            mount_root_path,
            Entry::create_directory_in("/var/lib/seedbank/seeds/foo/mounts", "config"),
        );

        let mut folder = MockFolder::new();
        folder
            .given_folder_entries(
                mount_root_path,
                vec![Entry::create_directory_in(mount_root_path, "dynamic")],
            )
            .given_folder_entries(
                subdir_path,
                vec![Entry::create_file_entry_in(subdir_path, "app.yml")],
            )
            .given_relative_path_with(mount_root_path, file_path, "dynamic/app.yml");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_bytes_with_contents(file_path, b"abc");

        let mut server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );
        server.inspect = Arc::new(inspect);

        let mut mount = mount(HashSet::new());
        server
            .hydrate_mount_contents(&name("foo"), &name("config"), &mut mount)
            .expect("should hydrate");

        let expected = MountContents::file("dynamic/app.yml", b"abc").expect("valid content");
        assert_eq!(mount.contents(), &HashSet::from([expected]));
    }

    #[test]
    fn test_hydrate_mount_contents_should_reject_a_symlinked_mount_root() {
        let mount_root_path = "/var/lib/seedbank/seeds/foo/mounts/config";

        let mut inspect = MockInspect::new();
        inspect.expect_entry_with_metadata(
            mount_root_path,
            Entry::create_symlink_in("/var/lib/seedbank/seeds/foo/mounts", "config"),
        );

        let mut server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.inspect = Arc::new(inspect);

        let mut mount = mount(HashSet::new());
        let result = server.hydrate_mount_contents(&name("foo"), &name("config"), &mut mount);

        assert!(matches!(
            result,
            Err(Error::FileSystemError(FileSystemError::InvalidPath(_)))
        ));
    }

    #[test]
    fn test_hydrate_mount_contents_should_reject_a_symlinked_child_entry() {
        let mount_root_path = "/var/lib/seedbank/seeds/foo/mounts/config";

        let mut inspect = MockInspect::new();
        inspect.expect_entry_with_metadata(
            mount_root_path,
            Entry::create_directory_in("/var/lib/seedbank/seeds/foo/mounts", "config"),
        );

        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            mount_root_path,
            vec![Entry::create_symlink_in(mount_root_path, "sneaky")],
        );

        let mut server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.inspect = Arc::new(inspect);

        let mut mount = mount(HashSet::new());
        let result = server.hydrate_mount_contents(&name("foo"), &name("config"), &mut mount);

        assert!(matches!(
            result,
            Err(Error::FileSystemError(FileSystemError::InvalidPath(_)))
        ));
    }

    #[test]
    fn test_clean_mounts_should_delete_a_file_removed_from_the_proposed_definition() {
        let current = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::file("static.yml", b"abc").expect("valid")
            ])),
        )]));
        let proposed = definition_with_mounts(mounts(vec![("config", mount(HashSet::new()))]));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_file_to_be_deleted("/var/lib/seedbank/seeds/foo/mounts/config/static.yml");

        let mut server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.file_deleter = Arc::new(file_deleter);

        server
            .purge_mounts(&name("foo"), Some(&current), &proposed)
            .expect("should clean");
    }

    #[test]
    fn test_clean_mounts_should_delete_a_file_whose_content_changed_so_it_can_be_rewritten() {
        let current = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::file("static.yml", b"old").expect("valid")
            ])),
        )]));
        let proposed = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::file("static.yml", b"new").expect("valid")
            ])),
        )]));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_file_to_be_deleted("/var/lib/seedbank/seeds/foo/mounts/config/static.yml");

        let mut server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.file_deleter = Arc::new(file_deleter);

        server
            .purge_mounts(&name("foo"), Some(&current), &proposed)
            .expect("should clean");
    }

    #[test]
    fn test_clean_mounts_should_delete_the_whole_mount_when_removed_from_the_proposed_definition() {
        let current = definition_with_mounts(mounts(vec![("config", mount(HashSet::new()))]));
        let proposed = definition_with_mounts(HashMap::new());

        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter.expect_folder_to_be_deleted("/var/lib/seedbank/seeds/foo/mounts/config");

        let server = build_server(
            MockFolder::new(),
            folder_deleter,
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        server
            .purge_mounts(&name("foo"), Some(&current), &proposed)
            .expect("should clean");
    }

    #[test]
    fn test_clean_mounts_should_use_folder_deleter_when_a_path_changes_from_folder_to_file() {
        let current = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::folder_only("dynamic").expect("valid")
            ])),
        )]));
        let proposed = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([MountContents::file(
                "dynamic",
                b"now a file",
            )
            .expect("valid")])),
        )]));

        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter
            .expect_folder_to_be_deleted("/var/lib/seedbank/seeds/foo/mounts/config/dynamic");

        let server = build_server(
            MockFolder::new(),
            folder_deleter,
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        server
            .purge_mounts(&name("foo"), Some(&current), &proposed)
            .expect("should clean");
    }

    #[test]
    fn test_clean_mounts_should_use_file_deleter_when_a_path_changes_from_file_to_folder() {
        let current = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([MountContents::file(
                "dynamic",
                b"was a file",
            )
            .expect("valid")])),
        )]));
        let proposed = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::folder_only("dynamic").expect("valid")
            ])),
        )]));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_file_to_be_deleted("/var/lib/seedbank/seeds/foo/mounts/config/dynamic");

        let mut server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );
        server.file_deleter = Arc::new(file_deleter);

        server
            .purge_mounts(&name("foo"), Some(&current), &proposed)
            .expect("should clean");
    }

    #[test]
    fn test_clean_mounts_should_not_delete_unchanged_content() {
        let shared = definition_with_mounts(mounts(vec![(
            "config",
            mount(HashSet::from([
                MountContents::file("static.yml", b"abc").expect("valid")
            ])),
        )]));

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        server
            .purge_mounts(&name("foo"), Some(&shared), &shared)
            .expect("should clean");
    }

    #[test]
    fn test_create_should_not_deadlock_by_locking_its_own_mutex_reentrantly() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");
        folder.given_folder_entries("/var/lib/seedbank/seeds", Vec::new());
        folder.expect_create_folder_recursively_with("/var/lib/seedbank/seeds/foo");

        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_to_file_with_contents(
                "/var/lib/seedbank/seeds/foo/seedling.toml",
                &toml::to_string(&definition()).expect("should serialize"),
            )
            .expect_write_to_file_with_contents("/var/lib/seedbank/seeds/foo/id", "0")
            .expect_write_to_file_with_contents("/var/lib/seedbank/seeds/foo/version", "0");

        let server = Arc::new(build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            file_writer,
        ));

        let (sender, receiver) = std::sync::mpsc::channel();
        let thread_server = Arc::clone(&server);
        std::thread::spawn(move || {
            let result = thread_server.create(&name("foo"), &version(0), &definition());
            let _ = sender.send(result);
        });

        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("create() should not hang by locking its own mutex reentrantly");

        assert!(result.is_ok());
    }

    #[test]
    fn test_default_should_be_none_when_unclaimed() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        assert_eq!(
            server.default_seedling().expect("should check default"),
            None
        );
    }

    #[test]
    fn test_default_should_return_the_claiming_seedling() {
        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        assert_eq!(
            server.default_seedling().expect("should check default"),
            Some(name("foo"))
        );
    }

    #[test]
    fn test_claim_default_should_fail_when_the_seedling_does_not_exist() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let result = server.claim_default(&name("foo"));

        assert!(matches!(result, Err(Error::NotFound(_))));
    }

    #[test]
    fn test_claim_default_should_write_the_default_file_when_unclaimed() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });

        let mut file_writer = MockFileWriter::new();
        file_writer.expect_write_to_file_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let server = build_server(folder, MockFolderDeleter::new(), file_reader, file_writer);

        server
            .claim_default(&name("foo"))
            .expect("should claim default");
    }

    #[test]
    fn test_claim_default_should_be_idempotent_for_the_current_owner() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        server
            .claim_default(&name("foo"))
            .expect("should be a no-op");
    }

    #[test]
    fn test_claim_default_should_reject_when_already_claimed_by_another_seedling() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/bar");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.claim_default(&name("bar"));

        assert!(
            matches!(result, Err(Error::DefaultAlreadyClaimed(current)) if current == name("foo"))
        );
    }

    #[test]
    fn test_release_default_should_delete_the_default_file_when_it_belongs_to_the_caller() {
        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_delete()
            .withf(|path| path == std::path::Path::new("/var/lib/seedbank/seeds/default"))
            .returning(|_| Ok(()));

        let (shutdown_sender, _) = broadcast::channel::<()>(1);
        let server = Server {
            reporter: Arc::new(NullReporter),
            folder: Arc::new(MockFolder::new()),
            folder_deleter: Arc::new(MockFolderDeleter::new()),
            file_deleter: Arc::new(file_deleter),
            file_reader: Arc::new(file_reader),
            file_writer: Arc::new(MockFileWriter::new()),
            inspect: Arc::new(MockInspect::new()),
            seeds: PathBuf::from("/var/lib/seedbank/seeds"),
            listener_factory: dummy_listener_factory("seedbank"),
            registration_listener_factory: dummy_listener_factory("seedbank-registration"),
            shutdown_sender,
            global_lock: std::sync::Mutex::new(()),
        };

        let result = server.release_default(&name("foo"));

        assert!(result.is_ok());
    }

    #[test]
    fn test_release_default_should_be_a_no_op_when_unclaimed() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        });

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.release_default(&name("foo"));

        assert!(result.is_ok());
    }

    #[test]
    fn test_release_default_should_be_a_no_op_when_claimed_by_another_seedling() {
        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents("/var/lib/seedbank/seeds/default", "foo");

        let server = build_server(
            MockFolder::new(),
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let result = server.release_default(&name("bar"));

        assert!(result.is_ok());
    }

    #[test]
    fn test_list_should_ignore_the_default_file() {
        let mut folder = MockFolder::new();
        folder.given_folder_entries(
            "/var/lib/seedbank/seeds",
            vec![
                Entry::create_directory("foo"),
                Entry::create_file_entry("default"),
            ],
        );

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            MockFileWriter::new(),
        );

        let names = server.list().expect("should list");

        assert_eq!(names, vec![name("foo")]);
    }
}
