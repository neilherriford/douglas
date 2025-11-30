use crate::{
    deferred_file_logger::DeferredFileLogger, file_logger::FileLogger, tee_logger::TeeLogger,
};
use bract::{Client, client::ClientError};
use config::{SystemPaths, create_system_paths};
use docker::{SimpleSystemClient, SystemClient};
use file_system::{
    FileAppender, LocalFileAppender, LocalFileReader, LocalPermissions, Permissions,
};
use log::Logger;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub struct StatusCommand {
    log: Box<dyn Logger>,
    system_paths: Box<dyn SystemPaths>,
    file_appender: Arc<dyn FileAppender>,
}

#[derive(Debug)]
pub enum BractStatus {
    NotRunning,
    NotIntialized,
    Status(bract::client::Status),
    CannotDetermineStatus(String),
}

#[derive(Debug)]
pub enum DockerStatus {
    Active,
    DockerClientError(String),
}

#[derive(Debug)]
pub struct DouglasStatus {
    pub bract_status: BractStatus,
    pub docker_status: DockerStatus,
}

impl StatusCommand {
    pub fn new() -> Self {
        let system_paths = create_system_paths();
        let log_path = system_paths.log_path("douglas");

        let file_appender: Arc<dyn FileAppender> = Arc::new(LocalFileAppender::new());
        Self {
            system_paths,
            file_appender: Arc::clone(&file_appender),
            log: Box::new(TeeLogger::new(Box::new(DeferredFileLogger::new(
                &log_path,
                file_appender,
            )))),
        }
    }

    pub fn perform(&self) -> bool {
        let bract_socket_path = self.system_paths.douglas_socket_path("bract");
        let token_path = self.system_paths.token_path();
        let file_reader = Box::new(LocalFileReader::new());

        let bract_logger = Box::new(FileLogger::new(
            self.system_paths.log_path("bract").as_path(),
            Arc::clone(&self.file_appender),
        ));

        let bract_client = Client::new(bract_logger, file_reader, bract_socket_path, token_path);
        let mut bract_status: BractStatus = BractStatus::NotRunning;

        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return false;
            }
        };
        rt.block_on(async {
            let response = bract_client.status().await;

            bract_status = match response {
                Ok(status) => BractStatus::Status(status),
                Err(ClientError::MissingToken) => BractStatus::NotIntialized,
                Err(ClientError::NoResponse | ClientError::ConnectionRefused) => {
                    BractStatus::NotRunning
                }
                Err(err) => BractStatus::CannotDetermineStatus(format!("{err:?}")),
            };
        });

        let docker_socket_path = self.system_paths.docker_socket_path();
        let docker_logger = Arc::new(FileLogger::new(
            self.system_paths.log_path("docker").as_path(),
            Arc::clone(&self.file_appender),
        ));

        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return false;
            }
        };

        let docker_status = rt.block_on(async move {
            match SimpleSystemClient::build(docker_socket_path, docker_logger).await {
                Ok(mut client) => match client.ping().await {
                    Ok(_) => DockerStatus::Active,
                    Err(err) => DockerStatus::DockerClientError(format!("{err:?}")),
                },
                Err(err) => DockerStatus::DockerClientError(format!("{err:?}")),
            }
        });

        let status = DouglasStatus {
            bract_status,
            docker_status,
        };
        self.log.info(&format!("{status:?}"));

        true
    }
}
