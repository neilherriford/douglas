use crate::directory::Directory;
use crate::os::{Os, OsError};
use crate::request_handler::{LocalRequestHandler, RequestHandler};
use crate::util::create_for_target;
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Listener, Modes, Permissions,
    UnixDomainSocket,
};
use log::Logger;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::{task, time};
use uuid::Uuid;

static FIVE_MINUTES: u64 = 5 * 60;
static RADICLE_USER: &str = "doug-radicle";
static RADICLE_GROUP: &str = "doug-radicle";
static DOUGLAS_GROUP: &str = "douglas";

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Must be root")]
    NotRootError,
    #[error("OS error {0}")]
    OsError(#[from] OsError),
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
}

trait Task<TResult, TError> {
    fn perform(&self) -> Result<TResult, TError>;
}

struct TokenRefresher {
    log: Arc<dyn Logger + 'static>,
    token_path: PathBuf,
    permissions: Arc<dyn Permissions + Sync + Send + 'static>,
    file_writer: Box<dyn FileWriter + Sync + Send + 'static>,
}

impl TokenRefresher {
    pub fn new(
        log: Arc<dyn Logger + 'static>,
        token_path: &Path,
        permissions: Arc<dyn Permissions + Sync + Send + 'static>,
        file_writer: impl FileWriter + Sync + Send + 'static,
    ) -> Self {
        Self {
            log: log.clone(),
            token_path: token_path.to_path_buf(),
            permissions: permissions.clone(),
            file_writer: Box::new(file_writer),
        }
    }
}

impl Task<(), FileSystemError> for TokenRefresher {
    fn perform(&self) -> Result<(), FileSystemError> {
        let token = Uuid::now_v7().to_string();
        self.file_writer
            .write_all(&self.token_path.clone(), token)?;

        self.permissions.change_user_and_group_ownership(
            &self.token_path,
            RADICLE_USER,
            RADICLE_GROUP,
        )?;

        self.permissions
            .change_mode(&self.token_path, &Modes::OwnerAndGroupReadWrite)?;
        self.log.info("Token refreshed");

        Ok(())
    }
}

struct CreateSystemDirectoryEntries {
    log: Arc<dyn Logger + 'static>,
    directory: Arc<dyn Directory>,
}

impl CreateSystemDirectoryEntries {
    pub fn new(log: Arc<dyn Logger + 'static>, directory: Arc<dyn Directory>) -> Self {
        Self {
            log: log,
            directory: directory,
        }
    }

    fn create_group(&self, name: &str) -> Result<(), OsError> {
        self.log.info(&format!("Creating '{}' group", name));
        self.directory.create_group(name)
    }
}

impl Task<(), OsError> for CreateSystemDirectoryEntries {
    fn perform(&self) -> Result<(), OsError> {
        self.create_group(DOUGLAS_GROUP)?;
        self.create_group(RADICLE_GROUP)?;

        self.log.info(&format!("creating '{}' user", RADICLE_USER));
        self.directory
            .create_user(RADICLE_USER, RADICLE_GROUP, vec![DOUGLAS_GROUP.to_string()])
    }
}

struct CreateListener {
    log: Arc<dyn Logger + 'static>,
    socket_path: PathBuf,
    file_deleter: Box<dyn FileDeleter>,
    permissions: Arc<dyn Permissions>,
    unix_domain_socket: Box<dyn UnixDomainSocket>,
}

impl CreateListener {
    pub fn new(
        log: Arc<dyn Logger + 'static>,
        socket_path: &Path,
        file_deleter: impl FileDeleter + 'static,
        permissions: Arc<dyn Permissions>,
        unix_domain_socket: impl UnixDomainSocket + 'static,
    ) -> Self {
        Self {
            log,
            socket_path: socket_path.to_path_buf(),
            file_deleter: Box::new(file_deleter),
            permissions,
            unix_domain_socket: Box::new(unix_domain_socket),
        }
    }
}

impl Task<Box<dyn Listener + Send + Sync + 'static>, ServerError> for CreateListener {
    fn perform(&self) -> Result<Box<dyn Listener + Send + Sync + 'static>, ServerError> {
        self.log.info(&format!(
            "Refreshing socket '{}'",
            self.socket_path.to_string_lossy()
        ));
        self.file_deleter.delete(&self.socket_path)?;

        let listener = self.unix_domain_socket.bind(&self.socket_path)?;

        self.permissions.change_user_and_group_ownership(
            &self.socket_path,
            RADICLE_USER,
            RADICLE_GROUP,
        )?;
        self.permissions
            .change_mode(&self.socket_path, &Modes::OwnerAndGroupReadWrite)?;

        self.log.info("  done!");
        Ok(listener)
    }
}

pub struct Server {
    log: Arc<dyn Logger + 'static>,
    request_handler: Arc<dyn RequestHandler + Sync + Send + 'static>,
    token_refresher: Arc<dyn Task<(), FileSystemError> + Sync + Send + 'static>,
    create_system_directory_entries: Box<dyn Task<(), OsError> + 'static>,
    create_listener:
        Box<dyn Task<Box<dyn Listener + Sync + Send + 'static>, ServerError> + 'static>,
    os: Arc<dyn Os + 'static>,
}

impl Server {
    pub fn new(
        log: Arc<dyn Logger + 'static>,
        file_reader: impl FileReader + Sync + Send + 'static,
        file_writer: impl FileWriter + Sync + Send + 'static,
        file_deleter: impl FileDeleter + Sync + Send + 'static,
        os: Arc<impl Os + 'static>,
        permissions: impl Permissions + Sync + Send + 'static,
        unix_domain_socket: impl UnixDomainSocket + 'static,
        token_path: &Path,
        socket_path: &Path,
    ) -> Self {
        let directory = Arc::new(create_for_target(os.clone()));
        let permissions = Arc::new(permissions);

        Self {
            log: log.clone(),
            request_handler: Arc::new(LocalRequestHandler::new(
                log.clone(),
                file_reader,
                directory.clone(),
                token_path,
            )),
            token_refresher: Arc::new(TokenRefresher::new(
                log.clone(),
                token_path,
                permissions.clone(),
                file_writer,
            )),
            create_system_directory_entries: Box::new(CreateSystemDirectoryEntries::new(
                log.clone(),
                directory.clone(),
            )),
            create_listener: Box::new(CreateListener::new(
                log.clone(),
                socket_path,
                file_deleter,
                permissions.clone(),
                unix_domain_socket,
            )),
            os,
        }
    }

    pub fn start(&self) -> Result<(), ServerError> {
        self.log.info("Starting server");
        self.assert_root()?;
        self.create_system_directory_entries.perform()?;

        let rt = tokio::runtime::Runtime::new()?;

        let log = self.log.clone();
        let os = self.os.clone();
        let token_refresher = self.token_refresher.clone();
        let request_handler = self.request_handler.clone();

        rt.block_on(async {
            let listner = self.create_listener.perform()?;
            let token_refresh_task = task::spawn(Self::token_refresh_task(
                log.clone(),
                token_refresher,
                os.clone(),
            ));
            let request_handler_task = task::spawn(Self::request_handler_task(
                log.clone(),
                request_handler,
                listner,
                os.clone(),
            ));

            let (_, handler_result) = tokio::try_join!(token_refresh_task, request_handler_task)?;

            handler_result
        })
    }

    async fn token_refresh_task(
        log: Arc<dyn Logger>,
        token_refresher: Arc<dyn Task<(), FileSystemError> + Send + Sync>,
        os: Arc<dyn Os>,
    ) {
        loop {
            if let Err(err) = token_refresher.perform() {
                log.error(&format!("Token refresh error: {:?}!  Exiting!", err));
                os.exit(-1);
            }
            time::sleep(Duration::from_secs(FIVE_MINUTES)).await;
        }
    }

    async fn request_handler_task(
        log: Arc<dyn Logger>,
        request_handler: Arc<dyn RequestHandler + Send + Sync + 'static>,
        listener: Box<dyn Listener + Send + Sync + 'static>,
        os: Arc<dyn Os>,
    ) -> Result<(), ServerError> {
        log.info("Listening…");
        loop {
            let (socket, _) = listener.accept().await?;
            log.info("Handling request");

            let handler = Arc::clone(&request_handler);
            let log = Arc::clone(&log);
            let os = Arc::clone(&os);

            tokio::spawn(async move {
                if let Err(err) = handler.handle(socket).await {
                    log.error(&format!("Handler error: {:?}!  Exiting!", err));
                    os.exit(-1);
                }
            });
        }
    }

    fn assert_root(&self) -> Result<(), ServerError> {
        if self.os.is_root() {
            Ok(())
        } else {
            self.log.error("Not root!");
            Err(ServerError::NotRootError)
        }
    }
}

#[cfg(test)]
mod tests {
    mod server {
        use super::super::*;
        use crate::os::MockOs;
        use file_system::{
            MockFileDeleter, MockFileReader, MockFileWriter, MockPermissions, MockUnixDomainSocket,
        };
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_err_if_not_root() {
            let mut log = MockLogger::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let mut os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();
            let token_path = Path::new("/tmp/token");
            let socket_path = Path::new("/tmp/socket");

            log.expect_info().returning(|_| ());
            log.expect_error()
                .with(predicate::eq("Not root!"))
                .returning(|_| ());
            os.expect_is_root().return_const(false);

            let actual = Server::new(
                Arc::new(log),
                file_reader,
                file_writer,
                file_deleter,
                Arc::new(os),
                permissions,
                unix_domain_socket,
                &token_path,
                &socket_path,
            )
            .start();

            assert!(matches!(actual, Err(ServerError::NotRootError)));
        }
    }

    mod token_refresher {
        use super::super::*;
        use file_system::{FileSystemError, MockFileWriter, MockPermissions, Modes};
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_error_if_write_error() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();

            log.expect_info().returning(|_| ());

            file_writer
                .expect_write_all()
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = TokenRefresher::new(
                Arc::new(log),
                &token_path,
                Arc::new(permissions),
                file_writer,
            )
            .perform();
            assert!(matches!(actual, Err(FileSystemError::ExpectedFileError)));
        }

        #[test]
        fn should_error_if_chown_error() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();

            log.expect_info().returning(|_| ());

            file_writer.expect_write_all().returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = TokenRefresher::new(
                Arc::new(log),
                &token_path,
                Arc::new(permissions),
                file_writer,
            )
            .perform();
            assert!(matches!(actual, Err(FileSystemError::ExpectedFileError)));
        }

        #[test]
        fn should_error_if_chmod_error() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();

            log.expect_info().returning(|_| ());

            file_writer.expect_write_all().returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .returning(|_, _, _| Ok(()));

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq(Modes::OwnerAndGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = TokenRefresher::new(
                Arc::new(log),
                &token_path,
                Arc::new(permissions),
                file_writer,
            )
            .perform();
            assert!(matches!(actual, Err(FileSystemError::ExpectedFileError)));
        }

        #[test]
        fn should_refresh() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();

            log.expect_info().returning(|_| ());

            file_writer.expect_write_all().returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .returning(|_, _, _| Ok(()));

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq(Modes::OwnerAndGroupReadWrite),
                )
                .returning(|_, _| Ok(()));

            let actual = TokenRefresher::new(
                Arc::new(log),
                &token_path,
                Arc::new(permissions),
                file_writer,
            )
            .perform();
            assert!(matches!(actual, Ok(())));
        }
    }
    mod create_system_directory_entries {
        use super::super::*;
        use crate::directory::MockDirectory;
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_err_if_douglas_group_fails() {
            let mut log = MockLogger::new();
            let mut directory = MockDirectory::new();

            log.expect_info().returning(|_| ());
            directory
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .return_once(|_| Err(OsError::InvalidName));

            let actual =
                CreateSystemDirectoryEntries::new(Arc::new(log), Arc::new(directory)).perform();
            assert!(matches!(actual, Err(OsError::InvalidName)));
        }

        #[test]
        fn should_err_if_radicle_group_fails() {
            let mut log = MockLogger::new();
            let mut directory = MockDirectory::new();

            log.expect_info().returning(|_| ());

            directory
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .return_once(|_| Ok(()));
            directory
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .return_once(|_| Err(OsError::InvalidName));

            let actual =
                CreateSystemDirectoryEntries::new(Arc::new(log), Arc::new(directory)).perform();
            assert!(matches!(actual, Err(OsError::InvalidName)));
        }

        #[test]
        fn should_err_if_radicle_user_fails() {
            let mut log = MockLogger::new();
            let mut directory = MockDirectory::new();

            log.expect_info().returning(|_| ());

            directory
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .return_once(|_| Ok(()));
            directory
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .return_once(|_| Ok(()));
            directory
                .expect_create_user()
                .with(
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .return_once(|_, _, _| Err(OsError::InvalidName));

            let actual =
                CreateSystemDirectoryEntries::new(Arc::new(log), Arc::new(directory)).perform();
            assert!(matches!(actual, Err(OsError::InvalidName)));
        }

        #[test]
        fn should_create_sytem_entries() {
            let mut log = MockLogger::new();
            let mut directory = MockDirectory::new();

            log.expect_info().returning(|_| ());

            directory
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .return_once(|_| Ok(()));
            directory
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .return_once(|_| Ok(()));
            directory
                .expect_create_user()
                .with(
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .return_once(|_, _, _| Ok(()));

            let actual =
                CreateSystemDirectoryEntries::new(Arc::new(log), Arc::new(directory)).perform();
            assert!(matches!(actual, Ok(())));
        }
    }

    mod create_listener {
        use super::super::*;
        use file_system::{MockFileDeleter, MockListener, MockPermissions, MockUnixDomainSocket};
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_err_if_cant_delete_socket() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().returning(|_| ());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                &socket_path,
                file_deleter,
                Arc::new(permissions),
                unix_domain_socket,
            )
            .perform();

            assert!(matches!(
                actual,
                Err(ServerError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }
        #[test]
        fn should_err_if_cant_bind() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().returning(|_| ());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(()));

            unix_domain_socket
                .expect_bind()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                &socket_path,
                file_deleter,
                Arc::new(permissions),
                unix_domain_socket,
            )
            .perform();

            assert!(matches!(
                actual,
                Err(ServerError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn should_err_if_permissions_failed() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().returning(|_| ());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(()));

            unix_domain_socket
                .expect_bind()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(Box::new(MockListener::new())));

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .return_once(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                &socket_path,
                file_deleter,
                Arc::new(permissions),
                unix_domain_socket,
            )
            .perform();

            assert!(matches!(
                actual,
                Err(ServerError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn should_err_if_ownership_permissions_failed() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().returning(|_| ());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(()));

            unix_domain_socket
                .expect_bind()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(Box::new(MockListener::new())));

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .return_once(|_, _, _| Ok(()));

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq(Modes::OwnerAndGroupReadWrite),
                )
                .return_once(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                &socket_path,
                file_deleter,
                Arc::new(permissions),
                unix_domain_socket,
            )
            .perform();

            assert!(matches!(
                actual,
                Err(ServerError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn should_create_listener() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().returning(|_| ());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(()));

            unix_domain_socket
                .expect_bind()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .return_once(|_| Ok(Box::new(MockListener::new())));

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                )
                .return_once(|_, _, _| Ok(()));

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq(Modes::OwnerAndGroupReadWrite),
                )
                .return_once(|_, _| Ok(()));

            let actual = CreateListener::new(
                Arc::new(log),
                &socket_path,
                file_deleter,
                Arc::new(permissions),
                unix_domain_socket,
            )
            .perform();

            assert!(matches!(actual, Ok(_)));
        }
    }
}
