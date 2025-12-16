use crate::{file_logger::FileLogger, tee_logger::TeeLogger};
use config::{SystemPaths, constants::DOUGLAS_ADMIN_GROUP, create_system_paths};
use credentials::{Credentials, create_credentials};
use daemonize::Daemonize;
use file_system::{
    FileAppender, FileDeleter, FileReader, FileWriter, Folder, Links, LocalFileAppender,
    LocalFileDeleter, LocalFileReader, LocalFileWriter, LocalFolder, LocalLinks, LocalPermissions,
    LocalUnixDomainSocket, Modes, Permissions, UnixDomainSocket, path_to_string,
};
use log::Logger;
use os::{Os, Unix};
use std::{fs::File, path::Path, sync::Arc};

pub struct StartBractDaemonCommand {
    credentials: Arc<dyn Credentials>,
    file_appender: Arc<dyn FileAppender>,
    file_deleter: Arc<dyn FileDeleter>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    folder: Arc<dyn Folder>,
    links: Arc<dyn Links>,
    log: Arc<dyn Logger>,
    os: Arc<dyn Os>,
    permissions: Arc<dyn Permissions>,
    system_paths: Arc<dyn SystemPaths>,
    unix_domain_socket: Arc<dyn UnixDomainSocket>,
}

impl StartBractDaemonCommand {
    pub fn new() -> Self {
        let system_paths: Arc<dyn SystemPaths> = Arc::from(create_system_paths());
        let file_appender: Arc<dyn FileAppender> = Arc::new(LocalFileAppender::new());
        let permissions: Arc<dyn Permissions> = Arc::new(LocalPermissions::new());

        let log = Arc::new(TeeLogger::new(Box::new(FileLogger::new(
            &system_paths.log_path("douglas"),
            Arc::clone(&file_appender),
        ))));

        let file_deleter = Arc::new(LocalFileDeleter::new());
        let file_reader = Arc::new(LocalFileReader::new());
        let file_writer = Arc::new(LocalFileWriter::new());
        let folder = Arc::new(LocalFolder::new());
        let links = Arc::new(LocalLinks::new());
        let unix_domain_socket = Arc::new(LocalUnixDomainSocket::new());
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials: Arc<dyn Credentials> = Arc::from(create_credentials(Arc::clone(&os)));

        Self {
            credentials,
            file_appender,
            file_deleter,
            file_reader,
            file_writer,
            folder,
            links,
            log,
            os,
            permissions,
            system_paths,
            unix_domain_socket,
        }
    }

    pub fn run(&self) -> bool {
        self.log.info("Starting bract…");

        let bract_stdout = unwrap_or_bail!(self.create_log_file("bract-stdout"));
        let bract_error = unwrap_or_bail!(self.create_log_file("bract-error"));

        let pid_file_path = self.system_paths.pid_path("bract");
        let daemonize = Daemonize::new()
            .pid_file(&pid_file_path)
            .working_directory(Path::new("/"))
            .stdout(bract_stdout)
            .stderr(bract_error);

        match daemonize.start() {
            Ok(()) => {
                bail_unless!(self.set_permissions_to_service_readable(&pid_file_path));
                let server = self.create_bract_server();

                if let Err(err) = server.start() {
                    self.log
                        .error(&format!("Failed to start bract server: {err:?}"));
                    return false;
                }
            }
            Err(err) => {
                self.log
                    .error(&format!("Failed to daemonize bract server: {err:?}"));
                return false;
            }
        }

        true
    }

    fn create_log_file(&self, log_name: &str) -> Option<File> {
        let log_path = self.system_paths.log_path(log_name);
        let file: File;

        if self.folder.exists(&log_path) {
            match self.folder.open_file_for_writing(&log_path) {
                Ok(opened) => file = opened,
                Err(err) => {
                    self.log
                        .error(&format!("Could not open {log_name}: {err:?}"));
                    return None;
                }
            }
        } else {
            match self.folder.create_file(&log_path) {
                Ok(created) => file = created,
                Err(err) => {
                    self.log
                        .error(&format!("Could not create {log_name}: {err:?}"));
                    return None;
                }
            }
        }

        if self.set_permissions_to_service_readable(&log_path) {
            Some(file)
        } else {
            None
        }
    }

    fn set_permissions_to_service_readable(&self, path: &Path) -> bool {
        let pretty_path = path_to_string(path);
        if let Err(err) = self.permissions.change_user_and_group_ownership(
            path,
            credentials::ROOT_USER_NAME,
            DOUGLAS_ADMIN_GROUP,
        ) {
            self.log.error(&format!(
                "Could not set ownership on {pretty_path}: {err:?}"
            ));
            return false;
        }

        self.permissions
            .change_mode(path, &Modes::OwnerReadWriteGroupRead)
            .map_err(|err| {
                self.log
                    .error(&format!("Could not set mode on {pretty_path}: {err:?}"));
            })
            .is_ok()
    }

    fn create_bract_server(&self) -> bract::Server {
        let logger: Arc<dyn Logger + Sync + Send> = Arc::new(FileLogger::new(
            &self.system_paths.log_path("bract"),
            Arc::clone(&self.file_appender),
        ));

        bract::Server::new(
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
            Arc::clone(&self.system_paths),
        )
    }
}
