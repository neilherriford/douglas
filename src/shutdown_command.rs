use config::{SystemPaths, create_system_paths};
use file_system::{
    FileAppender, LocalFileAppender, LocalFileReader, LocalPermissions, Permissions,
};
use log::Logger;
use std::sync::Arc;
use tokio::runtime::Runtime;

use crate::{
    deferred_file_logger::DeferredFileLogger, file_logger::FileLogger, tee_logger::TeeLogger,
};

pub struct ShutdownCommand {
    log: Box<dyn Logger>,
    file_appender: Arc<dyn FileAppender>,
    permissions: Arc<dyn Permissions>,
    system_paths: Box<dyn SystemPaths>,
}

impl ShutdownCommand {
    pub fn new() -> Self {
        let system_paths = create_system_paths();
        let log_path = system_paths.log_path("douglas");

        let file_appender: Arc<dyn FileAppender> = Arc::new(LocalFileAppender::new());
        let permissions: Arc<dyn Permissions> = Arc::new(LocalPermissions::new());

        Self {
            file_appender: Arc::clone(&file_appender),
            permissions: Arc::clone(&permissions),
            log: Box::new(TeeLogger::new(Box::new(DeferredFileLogger::new(
                &log_path,
                file_appender,
                permissions,
            )))),
            system_paths,
        }
    }

    pub fn perform(&self) -> bool {
        let bract_socket_path = self.system_paths.douglas_socket_path("bract");
        let token_path = self.system_paths.token_path();
        let file_reader = Box::new(LocalFileReader::new());

        let bract_logger = Box::new(FileLogger::new(
            self.system_paths.log_path("bract").as_path(),
            Arc::clone(&self.file_appender),
            Arc::clone(&self.permissions),
        ));

        let bract_client =
            bract::Client::new(bract_logger, file_reader, bract_socket_path, token_path);

        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                self.log.error(&format!("Runtime error: {err}"));
                return false;
            }
        };
        rt.block_on(async {
            match bract_client.shutdown().await {
                Ok(()) => true,
                Err(err) => {
                    self.log.error(&format!("Runtime error: {err}"));
                    false
                }
            }
        })
    }
}
