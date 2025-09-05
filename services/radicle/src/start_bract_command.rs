use crate::{
    bract_path_factory::BractPathFactory,
    config::{Config, ConfigReader, ConfigRepositoryError},
    constants,
    file_logger::FileLogger,
};
use bract::{Server, ServerError};
use credentials::Credentials;
use daemonize::Daemonize;
use file_system::{
    FileAppender, FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links, Modes,
    Permissions, UnixDomainSocket,
};
use log::Logger;
use os::{Os, OsError};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StartBractCommandError {
    #[error("Must be root to intialize the system")]
    NotRootError,
    #[error("Already running daemonized!")]
    AlreadyRunning,
    #[error("Daemon error '{0}'")]
    DaemonError(#[from] daemonize::Error),
    #[error("File system error '{0}'")]
    FileSystemError(#[from] FileSystemError),
    #[error("Configuration file error '{0}'")]
    ConfigRepositoryError(#[from] ConfigRepositoryError),
    #[error("Bract server error '{0}'")]
    BractServerError(#[from] ServerError),
    #[error("Os error '{0}'")]
    OsError(#[from] OsError),
    #[error("General error '{0}'")]
    GeneralError(String),
}

pub struct StartBractCommand {
    config_reader: Arc<dyn ConfigReader>,
    credentials: Arc<dyn Credentials + Sync + Send + 'static>,
    folder: Arc<dyn Folder + Sync + Send + 'static>,
    file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
    file_writer: Arc<dyn FileWriter + Sync + Send + 'static>,
    file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
    file_appender: Arc<dyn FileAppender + Sync + Send + 'static>,
    links: Arc<dyn Links + Sync + Send + 'static>,
    os: Arc<dyn Os + Sync + Send + 'static>,
    permissions: Arc<dyn Permissions + Sync + Send + 'static>,
    unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
    daemonize: bool,
    bract_path_factory: Arc<BractPathFactory>,
}

impl StartBractCommand {
    pub fn new(
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
        folder: Arc<dyn Folder + Sync + Send + 'static>,
        config_reader: Arc<dyn ConfigReader>,
        file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
        file_writer: Arc<dyn FileWriter + Sync + Send + 'static>,
        file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
        file_appender: Arc<dyn FileAppender + Sync + Send + 'static>,
        links: Arc<dyn Links + Sync + Send + 'static>,
        os: Arc<dyn Os + Sync + Send + 'static>,
        permissions: Arc<dyn Permissions + Sync + Send + 'static>,
        unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
        daemonize: bool,
        bract_path_factory: Arc<BractPathFactory>,
    ) -> Self {
        Self {
            credentials,
            config_reader,
            daemonize,
            folder,
            file_reader,
            file_writer,
            file_deleter,
            file_appender,
            links,
            os,
            permissions,
            unix_domain_socket,
            bract_path_factory,
        }
    }

    pub fn run(
        &self,
        override_logger: Option<Arc<dyn Logger + Send + Sync + 'static>>,
    ) -> Result<(), StartBractCommandError> {
        self.assert_root()?;
        self.assert_not_running()?;
        let config = self.config_reader.load()?;
        let server = self.create_server(&config, override_logger)?;

        if self.daemonize {
            self.run_detached(server, &config)?;
        } else {
            server.start()?
        }

        Ok(())
    }

    fn assert_root(&self) -> Result<(), StartBractCommandError> {
        if self.credentials.is_root() {
            Ok(())
        } else {
            Err(StartBractCommandError::NotRootError)
        }
    }

    fn create_server(
        &self,
        config: &Config,
        override_logger: Option<Arc<dyn Logger + Send + Sync + 'static>>,
    ) -> Result<Server, StartBractCommandError> {
        let log: Arc<dyn Logger + Sync + Send + 'static> = if let Some(log) = override_logger {
            log
        } else {
            let mut log_path = config.log_path.to_path_buf();
            log_path.push("douglas.log");
            let log_path = log_path.as_path();
            Arc::new(FileLogger::new(
                log_path,
                Arc::clone(&self.file_appender),
                Arc::clone(&self.permissions),
            ))
        };

        Ok(Server::new(
            Arc::clone(&log),
            Arc::clone(&self.file_reader),
            Arc::clone(&self.file_writer),
            Arc::clone(&self.file_deleter),
            Arc::clone(&self.folder),
            Arc::clone(&self.links),
            Arc::clone(&self.os),
            Arc::clone(&self.permissions),
            Arc::clone(&self.unix_domain_socket),
            Arc::clone(&self.credentials),
            self.bract_path_factory.token_path()?.as_path(),
            self.bract_path_factory.bract_socket_path()?.as_path(),
            &config.mount_root_path.as_path(),
            constants::RADICLE_USER,
            constants::RADICLE_GROUP,
            constants::DOUGLAS_GROUP,
        ))
    }

    fn run_detached(&self, server: Server, config: &Config) -> Result<(), StartBractCommandError> {
        let (stdout, stdout_path) = self
            .folder
            .create_file(&config.log_path.as_path(), "douglas-bract.out")?;
        let (stderr, stderr_path) = self
            .folder
            .create_file(&config.log_path.as_path(), "douglas-bract.err")?;

        self.set_permissions_to_service_readable(stdout_path.as_path())?;
        self.set_permissions_to_service_readable(stderr_path.as_path())?;

        let (working_directory, pid_file_path) = self.working_directory_and_pid_file_path()?;
        let daemonize = Daemonize::new()
            .pid_file(pid_file_path.as_path())
            .working_directory(working_directory.as_path())
            .stdout(stdout)
            .stderr(stderr);

        println!("🆗 Douglas bract started!");
        match daemonize.start() {
            Ok(_) => {
                self.set_permissions_to_service_readable(&pid_file_path)?;
                match server.start() {
                    Ok(()) => Ok(()),
                    Err(err) => Err(StartBractCommandError::BractServerError(err)),
                }
            }
            Err(err) => Err(StartBractCommandError::DaemonError(err)),
        }
    }

    fn working_directory_and_pid_file_path(
        &self,
    ) -> Result<(PathBuf, PathBuf), StartBractCommandError> {
        let executable_root = self.folder.executable_root()?;
        let mut pid_path = executable_root.to_path_buf();
        pid_path.push("bract.pid");
        Ok((executable_root, pid_path))
    }

    fn set_permissions_to_service_readable(&self, path: &Path) -> Result<(), FileSystemError> {
        self.permissions.change_user_and_group_ownership(
            path,
            credentials::ROOT_USER_NAME,
            constants::RADICLE_GROUP,
        )?;
        self.permissions
            .change_mode(path, &Modes::OwnerReadWriteGroupRead)
    }

    fn assert_not_running(&self) -> Result<(), StartBractCommandError> {
        if !self.daemonize {
            return Ok(());
        }

        let (_, pid_file_path) = self.working_directory_and_pid_file_path()?;

        let pid = self.file_reader.read_all(&pid_file_path)?;
        if let Some(pid) = pid.lines().next() {
            let pid: i32 = match pid.parse() {
                Ok(pid) => pid,
                Err(_) => {
                    return Err(StartBractCommandError::GeneralError(format!(
                        "Unexpectd PID format: {}",
                        pid
                    )));
                }
            };

            if self.os.is_active_pid(pid)? {
                Err(StartBractCommandError::AlreadyRunning)
            } else {
                Ok(())
            }
        } else {
            Err(StartBractCommandError::GeneralError(format!(
                "Could not determine bract pid from pid file: {:?}",
                pid_file_path
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StartBractCommand;
    use crate::{bract_path_factory::BractPathFactory, config::MockConfigReader};
    use credentials::Credentials;
    use file_system::{
        MockFileAppender, MockFileDeleter, MockFileReader, MockFileWriter, MockFolder, MockLinks,
        MockPermissions, MockUnixDomainSocket,
    };
    use os::MockOs;
    use std::sync::Arc;

    fn build(
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
        folder: Arc<MockFolder>,
        config_reader: Arc<MockConfigReader>,
        file_reader: Arc<MockFileReader>,
        file_writer: Arc<MockFileWriter>,
        file_deleter: Arc<MockFileDeleter>,
        file_appender: Arc<MockFileAppender>,
        links: Arc<MockLinks>,
        os: Arc<MockOs>,
        permissions: Arc<MockPermissions>,
        unix_domain_socket: Arc<MockUnixDomainSocket>,
        daemonize: bool,
    ) -> StartBractCommand {
        StartBractCommand::new(
            Arc::clone(&credentials),
            folder.clone(),
            config_reader.clone(),
            file_reader.clone(),
            file_writer.clone(),
            file_deleter.clone(),
            file_appender.clone(),
            links.clone(),
            os.clone(),
            permissions.clone(),
            unix_domain_socket.clone(),
            daemonize,
            Arc::new(BractPathFactory::new(folder.clone())),
        )
    }

    mod run {
        use super::build;
        use crate::{
            config::{Config, ConfigRepositoryError, MockConfigReader},
            start_bract_command::StartBractCommandError,
        };
        use credentials::MockCredentials;
        use file_system::{
            FileSystemError, MockFileAppender, MockFileDeleter, MockFileReader, MockFileWriter,
            MockFolder, MockLinks, MockPermissions, MockUnixDomainSocket,
        };
        use log::MockLogger;
        use mockall::predicate;
        use os::{MockOs, OsError};
        use std::{
            path::{Path, PathBuf},
            sync::Arc,
        };

        #[test]
        fn should_fail_if_not_root() {
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_not_root();

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(actual, Err(StartBractCommandError::NotRootError)))
        }

        #[test]
        fn should_error_if_pid_path_could_not_be_derived() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder
                .expect_executable_root()
                .returning(|| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::FileSystemError(_))
            ))
        }

        #[test]
        fn should_error_if_pid_file_could_not_be_read() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder.given_executable_root("/tmp");
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/bract.pid")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::FileSystemError(_))
            ))
        }

        #[test]
        fn should_error_if_pid_blank() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "");

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::GeneralError(_))
            ))
        }

        #[test]
        fn should_error_if_pid_is_not_a_number() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "oops");

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::GeneralError(_))
            ))
        }

        #[test]
        fn should_error_if_signal_fails() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let mut os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "12345");
            os.expect_is_active_pid()
                .with(predicate::eq(12345))
                .returning(|_| Err(OsError::ErrorNumber(-12345)));

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(actual, Err(StartBractCommandError::OsError(_))))
        }

        #[test]
        fn should_error_if_already_running() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let mut os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "12345");
            os.given_pid_is_active(12345);

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::AlreadyRunning)
            ))
        }

        #[test]
        fn should_err_if_config_cannot_be_read() {
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let mut config_reader = MockConfigReader::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();
            config_reader.expect_load().returning(|| {
                Err(ConfigRepositoryError::FileSystemError(
                    FileSystemError::ExpectedFileError,
                ))
            });

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                false,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::ConfigRepositoryError(_))
            ))
        }

        #[test]
        fn should_err_if_server_could_not_be_created() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut config_reader = MockConfigReader::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();
            config_reader.given_config(Config {
                operator_user: "foo".to_string(),
                operator_group: "bar".to_string(),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                log_path: PathBuf::from("/tmp/logs"),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });
            folder
                .expect_executable_root()
                .returning(|| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                false,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::FileSystemError(_))
            ))
        }

        #[test]
        fn should_err_if_std_out_log_could_not_be_created() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut config_reader = MockConfigReader::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let mut os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            credentials.given_is_root();
            config_reader.given_config(Config {
                operator_user: "foo".to_string(),
                operator_group: "bar".to_string(),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                log_path: PathBuf::from("/tmp/logs"),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });
            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "12345");
            os.given_pid_is_not_active(12345);

            folder
                .expect_create_file()
                .with(
                    predicate::eq(Path::new("/tmp/logs/")),
                    predicate::eq("douglas-bract.out"),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                true,
            )
            .run(None);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::FileSystemError(_))
            ))
        }

        #[test]
        fn should_use_override_logger() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut config_reader = MockConfigReader::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();
            let file_appender = MockFileAppender::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();
            let mut override_logger = MockLogger::new();

            credentials
                .given_is_root()
                .given_user_does_not_exist("doug-radicle");

            config_reader.given_config(Config {
                operator_user: "foo".to_string(),
                operator_group: "bar".to_string(),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                log_path: PathBuf::from("/tmp/logs"),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });
            folder.given_executable_root("/tmp");

            override_logger.expect_info().return_const(());
            let override_logger = Arc::new(override_logger);

            let actual = build(
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(config_reader),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
                Arc::new(file_appender),
                Arc::new(links),
                Arc::new(os),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
                false,
            )
            .run(Some(override_logger.clone()));

            assert!(matches!(
                actual,
                Err(StartBractCommandError::BractServerError(_))
            ))
        }
    }
}
