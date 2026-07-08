mod bootstrap;
mod protocol;

pub use bootstrap::{DOUGLAS_SEEDBANK_GROUP, DOUGLAS_SEEDBANK_USER, service_definition};
pub use protocol::{Request, Response};

use blueprint::listener::SocketListenerFactory;
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileReader, FileSystemError, FileWriter, Folder,
    FolderDeleter, Permissions, UnixDomainSocket, UnixFileDeleter, UnixFileReader, UnixFileWriter,
    UnixFolder, UnixFolderDeleter, UnixPermissions,
};
use log::{BufferedFileReporter, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};
use std::{
    path::PathBuf,
    str::FromStr,
    sync::{Arc, LazyLock},
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
    #[error("Failed to bootstrap")]
    FailedBoostrap(Vec<String>),
    #[error("Seedling already exists {0}")]
    AlreadyExists(Name),
    #[error("Seedling does not exist {0}")]
    NotFound(Name),

    #[error("IO Error {0}")]
    IoError(#[from] std::io::Error),
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Name parse error: {0}")]
    NameParseError(#[from] NameParseError),
    #[error("Seedling serialization error: {0}")]
    SeedlingSerializationError(#[from] toml::ser::Error),
    #[error("Seedling deserialization error: {0}")]
    SeedlingDeserializationError(#[from] toml::de::Error),
}

#[derive(Debug, Error)]
pub enum NameParseError {
    #[error("Name cannot be empty")]
    CannotBeEmpty,
    #[error("Name too long")]
    TooLong,
    #[error("Name is invalid")]
    InvalidName,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Name {
    value: String,
}

impl Name {
    fn assert_is_valid(value: &str) -> Result<(), NameParseError> {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[._-][a-z0-9]+)*$").unwrap());

        if value.is_empty() {
            Err(NameParseError::CannotBeEmpty)
        } else if value.len() > 16 {
            Err(NameParseError::TooLong)
        } else if PATTERN.is_match(value) {
            Ok(())
        } else {
            Err(NameParseError::InvalidName)
        }
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

impl FromStr for Name {
    type Err = NameParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::assert_is_valid(s)?;
        Ok(Self {
            value: s.to_string(),
        })
    }
}

impl Serialize for Name {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Name::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Seedling {
    name: Name,
}

impl std::fmt::Display for Seedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name.to_string())
    }
}

#[cfg_attr(test, mockall::automock)]
pub trait Seedbank {
    fn list(&self) -> Result<Vec<Name>, Error>;
    fn exists(&self, name: &Name) -> Result<bool, Error>;
    fn load(&self, name: &Name) -> Result<Seedling, Error>;
    fn create(&self, seedling: &Seedling) -> Result<(), Error>;
    fn delete(&self, name: &Name) -> Result<(), Error>;
    fn update(&self, seedling: &Seedling) -> Result<(), Error>;
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    folder: Arc<dyn Folder>,
    folder_deleter: Arc<dyn FolderDeleter>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    seeds: PathBuf,
    listener_factories: Vec<SocketListenerFactory>,
    shutdown_sender: Sender<()>,
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

        let listener_factories = service_definition(&douglas_folders)
            .owned_sockets
            .into_iter()
            .map(|socket_definition| {
                SocketListenerFactory::new(
                    socket_definition,
                    Arc::clone(&file_deleter),
                    Arc::clone(&permissions),
                    Arc::clone(&unix_domain_socket),
                )
            })
            .collect();

        Ok(Self {
            reporter,
            folder,
            folder_deleter,
            file_reader,
            file_writer,
            seeds,
            listener_factories,
            shutdown_sender,
        })
    }

    fn create_seedling_path(&self, name: &Name) -> PathBuf {
        let mut expected_path = self.seeds.clone();
        expected_path.push(name.to_string());
        expected_path
    }

    fn save_seedling(&self, seedling: &Seedling) -> Result<(), Error> {
        let path = self.create_seedling_path(&seedling.name);
        self.folder.create_recursively(&path)?;
        let contents = toml::to_string(seedling)?;
        let mut manifest_path = path;
        manifest_path.push("seedling.toml");
        self.file_writer.write_all(&manifest_path, &contents)?;
        Ok(())
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Error> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting seedbank",
            ScopeKind::Group,
        );

        let mut shutdown = self.shutdown_sender.subscribe();

        let listeners: Vec<_> = self
            .listener_factories
            .iter()
            .map(|factory| factory.create(&span))
            .collect::<Result<_, _>>()?;

        let accept_loops = async {
            let tasks: Vec<_> = listeners
                .into_iter()
                .map(|listener| {
                    let server = Arc::clone(&self);
                    tokio::spawn(async move { Self::accept_loop(listener, server).await })
                })
                .collect();

            for task in tasks {
                task.await.map_err(std::io::Error::other)??;
            }
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

        let Ok(mut serialized) = serde_json::to_string(&response) else {
            return;
        };
        serialized.push('\n');

        let _ = writer.write_all(serialized.as_bytes()).await;
    }
}

impl Seedbank for Server {
    fn list(&self) -> Result<Vec<Name>, Error> {
        Ok(self
            .folder
            .entries(&self.seeds)?
            .iter()
            .filter_map(|entry| Name::from_str(&entry.name).ok())
            .collect())
    }

    fn exists(&self, name: &Name) -> Result<bool, Error> {
        let expected_path = self.create_seedling_path(name);
        Ok(self.folder.exists(&expected_path))
    }

    fn create(&self, seedling: &Seedling) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Creating seedling {}…", seedling.name),
            ScopeKind::Task,
        )
        .start_guard();

        if self.exists(&seedling.name)? {
            return guard.finish(Err(Error::AlreadyExists(seedling.name.clone())));
        }

        guard.finish(self.save_seedling(seedling))
    }

    fn load(&self, name: &Name) -> Result<Seedling, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Loading seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();

        if !self.exists(name)? {
            return guard.finish(Err(Error::NotFound(name.clone())));
        }

        let mut path = self.create_seedling_path(name);
        path.push("seedling.toml");
        let raw = self.file_reader.read_all(&path)?;
        let seedling = toml::from_str::<Seedling>(&raw)?;

        guard.finish(Ok(seedling))
    }

    fn delete(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Deleting seedling {name}…"),
            ScopeKind::Task,
        )
        .start_guard();

        if !self.exists(name)? {
            return guard.finish(Err(Error::NotFound(name.clone())));
        }
        let path = self.create_seedling_path(name);
        self.folder_deleter.delete(&path)?;

        guard.finish(Ok(()))
    }

    fn update(&self, seedling: &Seedling) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Updating seedling {}…", seedling.name),
            ScopeKind::Task,
        )
        .start_guard();

        if !self.exists(&seedling.name)? {
            return Err(Error::NotFound(seedling.name.clone()));
        }

        guard.finish(self.save_seedling(seedling))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{Entry, MockFileReader, MockFileWriter, MockFolder, MockFolderDeleter};
    use log::Event;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("valid name")
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
            file_reader: Arc::new(file_reader),
            file_writer: Arc::new(file_writer),
            seeds: PathBuf::from("/var/lib/seedbank/seeds"),
            listener_factories: Vec::new(),
            shutdown_sender,
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
    fn test_create_should_write_seedling_manifest() {
        let seedling = Seedling { name: name("foo") };

        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");
        folder.expect_create_folder_recursively_with("/var/lib/seedbank/seeds/foo");

        let mut file_writer = MockFileWriter::new();
        file_writer.expect_write_to_file_with_contents(
            "/var/lib/seedbank/seeds/foo/seedling.toml",
            &toml::to_string(&seedling).expect("should serialize"),
        );

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            file_writer,
        );

        assert!(server.create(&seedling).is_ok());
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

        let result = server.create(&Seedling { name: name("foo") });

        assert!(matches!(result, Err(Error::AlreadyExists(_))));
    }

    #[test]
    fn test_load_should_parse_seedling_manifest() {
        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");

        let mut file_reader = MockFileReader::new();
        file_reader.given_can_read_all_with_contents(
            "/var/lib/seedbank/seeds/foo/seedling.toml",
            "name = \"foo\"\n",
        );

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            file_reader,
            MockFileWriter::new(),
        );

        let seedling = server.load(&name("foo")).expect("should load");

        assert_eq!(seedling.name, name("foo"));
    }

    #[test]
    fn test_load_should_fail_when_missing() {
        let mut folder = MockFolder::new();
        folder.given_does_not_exist("/var/lib/seedbank/seeds/foo");

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
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
        let seedling = Seedling { name: name("foo") };

        let mut folder = MockFolder::new();
        folder.given_exists("/var/lib/seedbank/seeds/foo");
        folder.expect_create_folder_recursively_with("/var/lib/seedbank/seeds/foo");

        let mut file_writer = MockFileWriter::new();
        file_writer.expect_write_to_file_with_contents(
            "/var/lib/seedbank/seeds/foo/seedling.toml",
            &toml::to_string(&seedling).expect("should serialize"),
        );

        let server = build_server(
            folder,
            MockFolderDeleter::new(),
            MockFileReader::new(),
            file_writer,
        );

        assert!(server.update(&seedling).is_ok());
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

        let result = server.update(&Seedling { name: name("foo") });

        assert!(matches!(result, Err(Error::NotFound(_))));
    }
}
