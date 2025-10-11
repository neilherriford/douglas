use crate::{
    bract_path_factory::BractPathFactory,
    config::{ConfigReader, ConfigRepositoryError},
};
use bract::{Client, client::ClientError};
use docker::{SimpleSystemClient, SystemClient};
use file_system::{FileReader, FileSystemError};
use log::Logger;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub struct StatusCommand {
    file_reader: Arc<dyn FileReader>,
    log: Arc<dyn Logger>,
    bract_path_factory: Arc<BractPathFactory>,
    config_reader: Arc<dyn ConfigReader>,
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
    ConfigFileNotFound,
    DockerClientError(String),
    CouldNotLoadConfiguration(String),
}

pub struct DouglasStatus {
    pub bract_status: BractStatus,
    pub docker_status: DockerStatus,
}

impl StatusCommand {
    pub fn new(
        file_reader: Arc<dyn FileReader>,
        log: Arc<dyn Logger>,
        bract_path_factory: Arc<BractPathFactory>,
        config_reader: Arc<dyn ConfigReader>,
    ) -> Self {
        let _ = config_reader;
        Self {
            file_reader,
            log,
            bract_path_factory,
            config_reader,
        }
    }

    pub fn perform(&self) -> Result<DouglasStatus, FileSystemError> {
        let bract_socket_path = self.bract_path_factory.bract_socket_path()?;
        let bract_socket_path = bract_socket_path.as_path();
        let token_path = self.bract_path_factory.token_path()?;
        let token_path = token_path.as_path();

        let bract_client = Client::new(
            Arc::clone(&self.log),
            Arc::clone(&self.file_reader),
            bract_socket_path,
            token_path,
        );

        let mut bract_status: BractStatus = BractStatus::NotRunning;

        let rt = Runtime::new()?;
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

        let docker_socket_path = match self.config_reader.read() {
            Ok(config) => config.docker_socket_path,
            Err(ConfigRepositoryError::NotFound) => {
                return Ok(DouglasStatus {
                    bract_status,
                    docker_status: DockerStatus::ConfigFileNotFound,
                });
            }
            Err(err) => {
                return Ok(DouglasStatus {
                    bract_status,
                    docker_status: DockerStatus::CouldNotLoadConfiguration(format!("{err:?}")),
                });
            }
        };

        let rt = Runtime::new()?;
        let docker_status = rt.block_on(async move {
            match SimpleSystemClient::build(
                docker_socket_path,
                Arc::clone(&self.log) as Arc<dyn Logger>,
            )
            .await
            {
                Ok(mut client) => match client.ping().await {
                    Ok(_) => DockerStatus::Active,
                    Err(err) => DockerStatus::DockerClientError(format!("{err:?}")),
                },
                Err(err) => DockerStatus::DockerClientError(format!("{err:?}")),
            }
        });

        Ok(DouglasStatus {
            bract_status,
            docker_status,
        })
    }
}
