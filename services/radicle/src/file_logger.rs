use chrono::Utc;
use file_system::{FileAppender, Modes, Permissions};
use log::Logger;
use std::sync::Once;
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::constants;

pub struct FileLogger {
    path: PathBuf,
    file_appender: Arc<dyn FileAppender + Send + Sync + 'static>,
    permissions: Arc<dyn Permissions + Send + Sync + 'static>,
    set_permissions: Once,
}

impl FileLogger {
    pub fn new(
        path: &Path,
        file_appender: Arc<dyn FileAppender + Send + Sync + 'static>,
        permissions: Arc<dyn Permissions + Send + Sync + 'static>,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            file_appender,
            permissions,
            set_permissions: Once::new(),
        }
    }

    fn log(&self, flag: &str, message: &str) {
        let now = Utc::now().to_rfc3339();

        if let Err(err) = self
            .file_appender
            .append(&self.path, format!("{},{},{}\n", now, flag, message))
        {
            eprintln!(
                "Error writing log: '{}' Original log entry: '{},{},{}'",
                err, now, flag, message
            );
        } else {
            self.set_permissions.call_once(|| {
                if let Err(err) = self.permissions.change_user_and_group_ownership(
                    &self.path,
                    credentials::ROOT_GROUP_NAME,
                    constants::RADICLE_GROUP,
                ) {
                    eprintln!("Failed to set permissions on log file! {}", err);
                } else {
                    if let Err(err) = self
                        .permissions
                        .change_mode(&self.path, &Modes::OwnerReadWriteGroupRead)
                    {
                        eprintln!("Failed to set mode on log file! {}", err);
                    }
                };
            });
        }
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
    use file_system::{MockFileAppender, MockPermissions};
    use std::{path::Path, sync::Arc};

    fn build(
        path: &str,
        file_appender: Arc<MockFileAppender>,
        permissions: Arc<MockPermissions>,
    ) -> FileLogger {
        FileLogger::new(Path::new(path), file_appender.clone(), permissions.clone())
    }

    mod log {
        use file_system::{MockFileAppender, MockPermissions, Modes};
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

        use super::build;

        #[test]
        fn should_set_permissions_once() {
            let mut file_appender = MockFileAppender::new();
            let mut permismisions = MockPermissions::new();

            file_appender.expect_append().returning(|_, _| Ok(()));
            permismisions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/log")),
                    predicate::eq("root"),
                    predicate::eq("doug-radicle"),
                )
                .times(1)
                .returning(|_, _, _| Ok(()));
            permismisions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/log")),
                    predicate::eq(Modes::OwnerReadWriteGroupRead),
                )
                .times(1)
                .returning(|_, _| Ok(()));

            let logger = build("/tmp/log", Arc::new(file_appender), Arc::new(permismisions));
            logger.log("foo", "bar");
            logger.log("baz", "qux");
        }
    }
}
