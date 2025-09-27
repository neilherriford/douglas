use crate::bract_path_factory::BractPathFactory;
use bract::{Client, client::ClientError};
use file_system::{FileReader, FileSystemError};
use log::Logger;
use std::sync::Arc;
use tokio::runtime::Runtime;

pub struct StatusCommand {
    file_reader: Arc<dyn FileReader + Send + Sync>,
    log: Arc<dyn Logger + Sync + Send>,
    bract_path_factory: Arc<BractPathFactory>,
}

pub enum BractStatus {
    NotIntialized,
    NotRunning,
    Status(bract::client::Status),
    CannotDetermineStatus,
}

pub struct DouglasStatus {
    pub bract_status: BractStatus,
}

impl StatusCommand {
    pub fn new(
        file_reader: Arc<dyn FileReader + Send + Sync>,
        log: Arc<dyn Logger + Sync + Send>,
        bract_path_factory: Arc<BractPathFactory>,
    ) -> Self {
        Self {
            file_reader,
            log,
            bract_path_factory,
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

        let mut bract_status: BractStatus = BractStatus::CannotDetermineStatus;

        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let response = bract_client.status().await;

            bract_status = match response {
                Ok(status) => BractStatus::Status(status),
                Err(ClientError::MissingToken) => BractStatus::NotIntialized,
                Err(ClientError::NoResponse) => BractStatus::NotRunning,
                Err(ClientError::ConnectionRefused) => BractStatus::NotRunning,
                Err(err) => {
                    eprintln!("{:?}", err);
                    BractStatus::CannotDetermineStatus
                }
            };
        });

        Ok(DouglasStatus { bract_status })
    }
}
