use crate::{
    bract_path_factory::BractPathFactory,
    config::{Config, ConfigReader, ConfigRepositoryError},
    constants, douglas_logger_factory,
    verbose_printer::VerbosePrinter,
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
    credentials: Arc<dyn Credentials + Send + Sync>,
    folder: Arc<dyn Folder + Send + Sync>,
    file_reader: Arc<dyn FileReader + Send + Sync>,
    file_writer: Arc<dyn FileWriter + Send + Sync>,
    file_deleter: Arc<dyn FileDeleter + Send + Sync>,
    file_appender: Arc<dyn FileAppender + Send + Sync>,
    links: Arc<dyn Links + Send + Sync>,
    os: Arc<dyn Os>,
    permissions: Arc<dyn Permissions + Send + Sync>,
    unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
    bract_path_factory: Arc<BractPathFactory>,
    logger: BractLogger,
    verbose_printer: Arc<dyn VerbosePrinter>,
}

pub enum BractLogger {
    Use(Arc<dyn Logger>),
    WriteToFile,
}

impl StartBractCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        credentials: Arc<dyn Credentials + Send + Sync>,
        folder: Arc<dyn Folder + Send + Sync>,
        config_reader: Arc<dyn ConfigReader>,
        file_reader: Arc<dyn FileReader + Send + Sync>,
        file_writer: Arc<dyn FileWriter + Send + Sync>,
        file_deleter: Arc<dyn FileDeleter + Send + Sync>,
        file_appender: Arc<dyn FileAppender + Send + Sync>,
        links: Arc<dyn Links + Send + Sync>,
        os: Arc<dyn Os>,
        permissions: Arc<dyn Permissions + Send + Sync>,
        unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
        bract_path_factory: Arc<BractPathFactory>,
        logger: BractLogger,
        verbose_printer: Arc<dyn VerbosePrinter>,
    ) -> Self {
        Self {
            config_reader,
            credentials,
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
            logger,
            verbose_printer,
        }
    }

    pub fn perform(&self, daemonize: bool) -> Result<(), StartBractCommandError> {
        self.verbose_printer.print("🌲 Starting bract…");
        self.verbose_printer.print_indented(1, "Starting…");
        self.verbose_printer
            .print_indented(1, "Verifiying credentials…");
        self.assert_root()?;
        self.verbose_printer
            .print_indented(1, "Checking if bract is already running…");
        self.assert_not_running(daemonize)?;
        self.verbose_printer.print_indented(1, "Loading config…");
        let config = self.config_reader.read()?;
        self.verbose_printer
            .print_indented(1, "Initializing server…");

        let server = self.create_server(&config)?;
        if daemonize {
            self.verbose_printer
                .print_indented(1, "Starting daemonizeds…");
            self.run_detached(&server, &config)?;
        } else {
            server.start()?;
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

    fn create_server(&self, config: &Config) -> Result<Server, StartBractCommandError> {
        let logger = match &self.logger {
            BractLogger::Use(logger) => Arc::clone(logger),
            BractLogger::WriteToFile => douglas_logger_factory::create(
                config,
                Arc::clone(&self.file_appender),
                Arc::clone(&self.permissions),
            ),
        };

        Ok(Server::new(
            logger,
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
            config.mount_root_path.as_path(),
            constants::RADICLE_USER,
            constants::RADICLE_GROUP,
            constants::DOUGLAS_GROUP,
        ))
    }

    fn run_detached(&self, server: &Server, config: &Config) -> Result<(), StartBractCommandError> {
        let (stdout, stdout_path) = self
            .folder
            .create_file(config.log_path.as_path(), "douglas-bract.out")?;
        let (stderr, stderr_path) = self
            .folder
            .create_file(config.log_path.as_path(), "douglas-bract.err")?;

        self.set_permissions_to_service_readable(stdout_path.as_path())?;
        self.set_permissions_to_service_readable(stderr_path.as_path())?;

        let (working_directory, pid_file_path) = self.working_directory_and_pid_file_path()?;
        let daemonize = Daemonize::new()
            .pid_file(pid_file_path.as_path())
            .working_directory(working_directory.as_path())
            .stdout(stdout)
            .stderr(stderr);

        self.verbose_printer.print("🆗 Douglas bract started!");
        match daemonize.start() {
            Ok(()) => {
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
        let mut pid_path = executable_root.clone();
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

    fn assert_not_running(&self, daemonize: bool) -> Result<(), StartBractCommandError> {
        if !daemonize {
            return Ok(());
        }

        let (_, pid_file_path) = self.working_directory_and_pid_file_path()?;

        if !self.folder.exists(pid_file_path.as_path()) {
            return Ok(());
        }

        let pid = self.file_reader.read_all(&pid_file_path)?;
        if let Some(pid) = pid.lines().next() {
            let pid: i32 = match pid.parse() {
                Ok(pid) => pid,
                Err(_) => {
                    return Err(StartBractCommandError::GeneralError(format!(
                        "Unexpected PID format: {pid}"
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
                "Could not determine bract pid from pid file: {pid_file_path:?}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BractLogger, StartBractCommand};
    use crate::{
        bract_path_factory::BractPathFactory, config::MockConfigReader,
        verbose_printer::MockVerbosePrinter,
    };
    use credentials::{Credentials, MockCredentials};
    use file_system::{
        MockFileAppender, MockFileDeleter, MockFileReader, MockFileWriter, MockFolder, MockLinks,
        MockPermissions, MockUnixDomainSocket,
    };
    use os::MockOs;
    use std::sync::Arc;

    #[allow(clippy::too_many_arguments)]
    fn build(
        credentials: &Arc<MockCredentials>,
        folder: &Arc<MockFolder>,
        config_reader: &Arc<MockConfigReader>,
        file_reader: &Arc<MockFileReader>,
        file_writer: &Arc<MockFileWriter>,
        file_deleter: &Arc<MockFileDeleter>,
        file_appender: &Arc<MockFileAppender>,
        links: &Arc<MockLinks>,
        os: &Arc<MockOs>,
        permissions: &Arc<MockPermissions>,
        unix_domain_socket: &Arc<MockUnixDomainSocket>,
        verbose_printer: &Arc<MockVerbosePrinter>,
        logger: BractLogger,
    ) -> StartBractCommand {
        let credentials: Arc<dyn Credentials + Sync + Send + 'static> =
            Arc::clone(credentials) as Arc<dyn Credentials + Sync + Send + 'static>;

        StartBractCommand::new(
            credentials,
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
            Arc::new(BractPathFactory::new(folder.clone())),
            logger,
            verbose_printer.clone(),
        )
    }

    mod perform {
        use super::build;
        use crate::{
            config::{ConfigRepositoryError, MockConfigReader},
            start_bract_command::{BractLogger, StartBractCommandError},
            verbose_printer::MockVerbosePrinter,
        };
        use credentials::MockCredentials;
        use file_system::{
            FileSystemError, MockFileAppender, MockFileDeleter, MockFileReader, MockFileWriter,
            MockFolder, MockLinks, MockPermissions, MockUnixDomainSocket,
        };
        use log::{Logger, MockLogger};
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_error_if_not_root() {
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
            let mut verbose_printer = MockVerbosePrinter::new();
            let logger = MockLogger::new();

            verbose_printer.expect_print().return_const(());
            verbose_printer.expect_print_indented().return_const(());
            credentials.given_is_not_root();

            let actual = build(
                &Arc::new(credentials),
                &Arc::new(folder),
                &Arc::new(config_reader),
                &Arc::new(file_reader),
                &Arc::new(file_writer),
                &Arc::new(file_deleter),
                &Arc::new(file_appender),
                &Arc::new(links),
                &Arc::new(os),
                &Arc::new(permissions),
                &Arc::new(unix_domain_socket),
                &Arc::new(verbose_printer),
                BractLogger::Use(Arc::new(logger) as Arc<dyn Logger + Send + Sync>),
            )
            .perform(false);

            assert!(matches!(actual, Err(StartBractCommandError::NotRootError)));
        }

        #[test]
        fn should_err_if_pid_is_invalid_when_running_daemonized() {
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
            let mut verbose_printer = MockVerbosePrinter::new();
            let logger = MockLogger::new();

            verbose_printer.expect_print().return_const(());
            verbose_printer.expect_print_indented().return_const(());
            credentials.given_is_root();
            folder
                .given_executable_root("/tmp")
                .given_exists("/tmp/bract.pid");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "oops");

            let actual = build(
                &Arc::new(credentials),
                &Arc::new(folder),
                &Arc::new(config_reader),
                &Arc::new(file_reader),
                &Arc::new(file_writer),
                &Arc::new(file_deleter),
                &Arc::new(file_appender),
                &Arc::new(links),
                &Arc::new(os),
                &Arc::new(permissions),
                &Arc::new(unix_domain_socket),
                &Arc::new(verbose_printer),
                BractLogger::Use(Arc::new(logger) as Arc<dyn Logger + Send + Sync>),
            )
            .perform(true);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::GeneralError(text))
                if text == "Unexpected PID format: oops"
            ));
        }

        #[test]
        fn should_err_if_already_running_when_running_daemonized() {
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
            let mut verbose_printer = MockVerbosePrinter::new();
            let logger = MockLogger::new();

            verbose_printer.expect_print().return_const(());
            verbose_printer.expect_print_indented().return_const(());
            credentials.given_is_root();
            folder
                .given_executable_root("/tmp")
                .given_exists("/tmp/bract.pid");
            file_reader.given_can_read_all_with_contents("/tmp/bract.pid", "12345");
            os.given_pid_is_active(12345);

            let actual = build(
                &Arc::new(credentials),
                &Arc::new(folder),
                &Arc::new(config_reader),
                &Arc::new(file_reader),
                &Arc::new(file_writer),
                &Arc::new(file_deleter),
                &Arc::new(file_appender),
                &Arc::new(links),
                &Arc::new(os),
                &Arc::new(permissions),
                &Arc::new(unix_domain_socket),
                &Arc::new(verbose_printer),
                BractLogger::Use(Arc::new(logger) as Arc<dyn Logger + Send + Sync>),
            )
            .perform(true);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::AlreadyRunning)
            ));
        }

        #[test]
        fn should_error_if_config_read_failed() {
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
            let mut verbose_printer = MockVerbosePrinter::new();
            let logger = MockLogger::new();

            verbose_printer.expect_print().return_const(());
            verbose_printer.expect_print_indented().return_const(());
            credentials.given_is_root();
            config_reader.expect_read().returning(|| {
                Err(ConfigRepositoryError::FileSystemError(
                    FileSystemError::ExpectedFileError,
                ))
            });

            let actual = build(
                &Arc::new(credentials),
                &Arc::new(folder),
                &Arc::new(config_reader),
                &Arc::new(file_reader),
                &Arc::new(file_writer),
                &Arc::new(file_deleter),
                &Arc::new(file_appender),
                &Arc::new(links),
                &Arc::new(os),
                &Arc::new(permissions),
                &Arc::new(unix_domain_socket),
                &Arc::new(verbose_printer),
                BractLogger::Use(Arc::new(logger) as Arc<dyn Logger + Send + Sync>),
            )
            .perform(false);

            assert!(matches!(
                actual,
                Err(StartBractCommandError::ConfigRepositoryError(_))
            ));
        }
    }
}
