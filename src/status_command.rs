use crate::{
    core_applications, deferred_file_logger::DeferredFileLogger, file_logger::FileLogger,
    tee_logger::TeeLogger,
};
use bract::{Client, client::ClientError};
use config::{SystemPaths, create_system_paths};
use docker::{SimpleSystemClient, SystemClient as DockerSystemClient};
use file_system::{FileAppender, LocalFileAppender, LocalFileReader};
use log::Logger;
use openbao::SystemClient as OpenBaoSystemClient;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub struct StatusCommand {
    log: Box<dyn Logger>,
    system_paths: Box<dyn SystemPaths>,
    file_appender: Arc<dyn FileAppender>,
}

#[derive(Debug, PartialEq)]
pub enum BractStatus {
    NotRunning,
    NotIntialized,
    Running(bract::client::Status),
    Error(String),
}

#[derive(Debug, PartialEq)]
pub enum OpenBaoStatus {
    NotRunning,
    Running {
        initialized: bool,
        sealed: bool,
        standby: bool,
    },
    Error(String),
}

#[derive(Debug, PartialEq)]
pub enum DockerStatus {
    Active,
    Error(String),
}

#[derive(Debug)]
pub struct DouglasStatus {
    pub bract_status: BractStatus,
    pub docker_status: DockerStatus,
    pub openbao_status: OpenBaoStatus,
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
        let bract_status = self.bract_status();
        let docker_status = self.docker_status();
        let openbao_status = if matches!(bract_status, BractStatus::Running(_))
            && docker_status == DockerStatus::Active
        {
            self.openbao_status()
        } else {
            OpenBaoStatus::NotRunning
        };

        let status = DouglasStatus {
            bract_status,
            docker_status,
            openbao_status,
        };
        self.log.info(&format!("{status:?}"));

        true
    }

    fn docker_status(&self) -> DockerStatus {
        let docker_socket_path = self.system_paths.docker_socket_path();
        let docker_logger = Arc::new(FileLogger::new(
            self.system_paths.log_path("docker").as_path(),
            Arc::clone(&self.file_appender),
        ));
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return DockerStatus::Error(err.to_string());
            }
        };
        rt.block_on(async move {
            match SimpleSystemClient::build(docker_socket_path, docker_logger).await {
                Ok(mut client) => match client.ping().await {
                    Ok(_) => DockerStatus::Active,
                    Err(err) => DockerStatus::Error(err.to_string()),
                },
                Err(err) => DockerStatus::Error(err.to_string()),
            }
        })
    }

    fn bract_status(&self) -> BractStatus {
        let bract_client = self.create_bract_client();
        let mut bract_status: BractStatus = BractStatus::NotRunning;
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return BractStatus::Error(err.to_string());
            }
        };
        rt.block_on(async {
            let response = bract_client.status().await;

            bract_status = match response {
                Ok(status) => BractStatus::Running(status),
                Err(ClientError::MissingToken) => BractStatus::NotIntialized,
                Err(ClientError::NoResponse | ClientError::ConnectionRefused) => {
                    BractStatus::NotRunning
                }
                Err(err) => BractStatus::Error(err.to_string()),
            };
        });

        bract_status
    }

    fn create_bract_client(&self) -> Client {
        let bract_socket_path = self.system_paths.douglas_socket_path("bract");
        let token_path = self.system_paths.token_path();
        let file_reader = Box::new(LocalFileReader::new());

        let bract_logger = Box::new(FileLogger::new(
            self.system_paths.log_path("bract").as_path(),
            Arc::clone(&self.file_appender),
        ));
        Client::new(bract_logger, file_reader, bract_socket_path, token_path)
    }

    fn openbao_status(&self) -> OpenBaoStatus {
        let openbao_logger: Arc<dyn Logger> = Arc::new(FileLogger::new(
            self.system_paths.log_path("openbao").as_path(),
            Arc::clone(&self.file_appender),
        ));

        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return OpenBaoStatus::Error(err.to_string());
            }
        };
        let bract_client = self.create_bract_client();

        rt.block_on(async {
            let openbao_definition = core_applications::OpenBao::new();

            let openbao_socket_path =
                match openbao_definition.qualified_socket_path(bract_client).await {
                    Ok(openbao_socket_path) => openbao_socket_path,
                    Err(err) => {
                        self.log
                            .error("Could not determine qualified socket path for OpenBao");
                        return OpenBaoStatus::Error(format!(
                            "Could not determine qualified socket path for OpenBao: {err}",
                        ));
                    }
                };

            let mut openbao_client =
                match openbao::SimpleSystemClient::build(openbao_socket_path, openbao_logger).await
                {
                    Ok(client) => client,
                    Err(err) => {
                        self.log.error(&format!("OpenBao client error: {err}"));
                        return OpenBaoStatus::Error(err.to_string());
                    }
                };

            match openbao_client.status().await {
                Ok(status) => OpenBaoStatus::Running {
                    initialized: status.initialized,
                    sealed: status.sealed,
                    standby: status.standby,
                },
                Err(err) => {
                    self.log.error(&format!("OpenBao client error: {err}"));
                    OpenBaoStatus::Error(err.to_string())
                }
            }
        })
    }
}
