use crate::{
    bract_path_factory::BractPathFactory,
    config::{ConfigReader, ConfigRepositoryError},
    douglas_logger_factory,
};
use bract::{Client, client::ClientError};
use file_system::{FileAppender, FileReader, FileSystemError, Permissions};
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

#[derive(Error, Debug)]
pub enum ShutdownCommandError {
    #[error("Client error: {0}")]
    CLient(#[from] ClientError),
    #[error("Configuration error: {0}")]
    ConfigRepository(#[from] ConfigRepositoryError),
    #[error("File system error '{0}'")]
    FileSystem(#[from] FileSystemError),
    #[error("IO error '{0}'")]
    Io(#[from] std::io::Error),
}

pub struct ShutdownCommand {
    bract_path_factory: Arc<BractPathFactory>,
    config_reader: Arc<dyn ConfigReader>,
    file_appender: Arc<dyn FileAppender + Send + Sync>,
    file_reader: Arc<dyn FileReader>,
    permissions: Arc<dyn Permissions + Send + Sync>,
}

impl ShutdownCommand {
    pub fn new(
        bract_path_factory: Arc<BractPathFactory>,
        config_reader: Arc<dyn ConfigReader>,
        file_appender: Arc<dyn FileAppender + Send + Sync>,
        file_reader: Arc<dyn FileReader>,
        permissions: Arc<dyn Permissions + Send + Sync>,
    ) -> Self {
        Self {
            bract_path_factory,
            config_reader,
            file_appender,
            file_reader,
            permissions,
        }
    }

    pub fn perform(&self) -> Result<(), ShutdownCommandError> {
        let config = self.config_reader.read()?;

        let logger = douglas_logger_factory::create(
            &config,
            Arc::clone(&self.file_appender),
            Arc::clone(&self.permissions),
        );

        let client = Client::new(
            logger,
            Arc::clone(&self.file_reader),
            self.bract_path_factory.bract_socket_path()?.as_path(),
            self.bract_path_factory.token_path()?.as_path(),
        );

        let rt = Runtime::new()?;
        rt.block_on(async {
            client.shutdown().await?;
            Result::<(), ClientError>::Ok(())
        })?;

        Ok(())
    }
}
