use std::sync::Arc;

use crate::{config::Config, file_logger::FileLogger};
use file_system::{FileAppender, Permissions};
use log::Logger;

pub fn create(
    config: &Config,
    file_appender: Arc<dyn FileAppender + Sync + Send>,
    permissions: Arc<dyn Permissions + Sync + Send>,
) -> Arc<dyn Logger> {
    let mut log_path = config.log_path.clone();
    log_path.push("douglas.log");
    let log_path = log_path.as_path();
    Arc::new(FileLogger::new(log_path, file_appender, permissions))
}
