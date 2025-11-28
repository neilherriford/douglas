use chrono::Utc;
use config::constants;
use file_system::{FileAppender, Modes, Permissions, path_to_string};
use log::Logger;
use std::sync::{Mutex, Once};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level::Debug => f.write_str("debug"),
            Level::Info => f.write_str("info"),
            Level::Warning => f.write_str("warning"),
            Level::Error => f.write_str("error"),
        }
    }
}

struct Entry {
    pub now: String,
    pub level: Level,
    pub message: String,
}

impl std::fmt::Display for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("{},{},{}\n", self.now, self.level, self.message))
    }
}

pub struct DeferredFileLogger {
    path: PathBuf,
    file_appender: Arc<dyn FileAppender>,
    permissions: Arc<dyn Permissions>,
    set_permissions: Once,
    buffer: Mutex<Vec<Entry>>,
}

impl DeferredFileLogger {
    pub fn new(
        path: &Path,
        file_appender: Arc<dyn FileAppender>,
        permissions: Arc<dyn Permissions>,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            file_appender,
            permissions,
            set_permissions: Once::new(),
            buffer: Mutex::new(Vec::new()),
        }
    }

    fn log(&self, level: Level, message: &str) {
        let entry = Entry {
            now: Utc::now().to_rfc3339(),
            level,
            message: message.to_string(),
        };

        if !self.file_appender.exists(&self.path) {
            match self.buffer.lock() {
                Ok(mut buffer) => buffer.push(entry),
                Err(err) => {
                    eprintln!("Error writing log: '{err}' Original log entry: '{entry}'");
                }
            }
            return;
        }

        self.flush_buffer();
        self.write_entry(&entry);
    }

    fn flush_buffer(&self) {
        match self.buffer.lock() {
            Ok(mut buffer) => {
                for entry in buffer.drain(..) {
                    self.write_entry(&entry);
                }
            }
            Err(err) => {
                eprintln!("Error flushing buffer: '{err}'");
            }
        }
    }

    fn write_entry(&self, entry: &Entry) {
        if let Err(err) = self.file_appender.append(&self.path, entry.to_string()) {
            eprintln!("Error writing log: '{err}' Original log entry: {entry}");
            return;
        }

        self.set_permissions.call_once(|| {
            if let Err(err) = self.permissions.change_user_and_group_ownership(
                &self.path,
                credentials::ROOT_GROUP_NAME,
                constants::RADICLE_GROUP,
            ) {
                eprintln!("Failed to set permissions on log file! {err}");
                return;
            }
            if let Err(err) = self
                .permissions
                .change_mode(&self.path, &Modes::OwnerReadWriteGroupRead)
            {
                eprintln!("Failed to set mode on log file! {err}");
            }
        });
    }
}

impl Logger for DeferredFileLogger {
    fn debug(&self, message: &str) {
        self.log(Level::Debug, message);
    }

    fn info(&self, message: &str) {
        self.log(Level::Info, message);
    }

    fn warn(&self, message: &str) {
        self.log(Level::Warning, message);
    }

    fn error(&self, message: &str) {
        self.log(Level::Error, message);
    }
}

impl Debug for DeferredFileLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "DeferredFileLogger ({})",
            path_to_string(&self.path)
        ))
    }
}

impl Drop for DeferredFileLogger {
    fn drop(&mut self) {
        self.flush_buffer();
    }
}
