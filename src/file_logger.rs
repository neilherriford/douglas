use chrono::Utc;
use file_system::FileAppender;
use log::Logger;
use std::{
    fmt::{Debug, Display},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct FileLogger {
    path: PathBuf,
    file_appender: Mutex<Arc<dyn FileAppender>>,
}

impl FileLogger {
    pub fn new(path: &Path, file_appender: Arc<dyn FileAppender>) -> Self {
        Self {
            path: path.to_path_buf(),
            file_appender: Mutex::new(file_appender),
        }
    }

    fn log(&self, flag: &str, message: &str) {
        let now = Utc::now().to_rfc3339();
        match self.file_appender.lock() {
            Ok(file_appender) => {
                if let Err(err) =
                    file_appender.append(&self.path, format!("{now},{flag},{message}\n"))
                {
                    Self::print_log_to_std_error(
                        err,
                        &now,
                        self.path.to_str().unwrap_or_default(),
                        flag,
                        message,
                    );
                }
            }
            Err(err) => {
                Self::print_log_to_std_error(
                    err,
                    &now,
                    self.path.to_str().unwrap_or_default(),
                    flag,
                    message,
                );
            }
        }
    }

    fn print_log_to_std_error(err: impl Display, now: &str, path: &str, flag: &str, message: &str) {
        eprintln!("Error writing log {path}: '{err}' Original log entry: '{now},{flag},{message}'",);
    }
}

impl Logger for FileLogger {
    fn debug(&self, message: &str) {
        self.log("debug", message);
    }

    fn info(&self, message: &str) {
        self.log("info", message);
    }

    fn warn(&self, message: &str) {
        self.log("warning", message);
    }

    fn error(&self, message: &str) {
        self.log("error", message);
    }
}

impl Debug for FileLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("FileLogger ({:?})", self.path))
    }
}

#[cfg(test)]
mod tests {
    use super::FileLogger;
    use file_system::MockFileAppender;
    use std::{path::Path, sync::Arc};

    fn build(path: &str, file_appender: &Arc<MockFileAppender>) -> FileLogger {
        FileLogger::new(Path::new(path), file_appender.clone())
    }

    mod log {
        use super::build;
        use file_system::MockFileAppender;
        use std::sync::Arc;

        #[test]
        fn should_set_permissions_once() {
            let mut file_appender = MockFileAppender::new();

            file_appender.expect_append().returning(|_, _| Ok(()));

            let logger = build("/tmp/log", &Arc::new(file_appender));
            logger.log("foo", "bar");
            logger.log("baz", "qux");
        }
    }
}
