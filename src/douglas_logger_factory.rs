use std::sync::Arc;

use crate::{config::Config, file_logger::FileLogger};
use file_system::{FileAppender, Permissions};
use log::Logger;

pub fn create(
    config: &Config,
    file_appender: Arc<dyn FileAppender + Send + Sync>,
    permissions: Arc<dyn Permissions + Send + Sync>,
) -> Arc<dyn Logger> {
    let mut log_path = config.log_path.to_path_buf();
    log_path.push("douglas.log");
    let log_path = log_path.as_path();
    Arc::new(FileLogger::new(log_path, file_appender, permissions))
}
