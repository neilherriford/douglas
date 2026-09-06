use file_system::{FileSystemError, FileWriter};
#[cfg(feature = "mock")]
use mockall::automock;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HeartbeatWriterError {
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Serialization error: {0}")]
    SupportFileSerializationError(#[from] serde_json::Error),
}

#[cfg_attr(feature = "mock", automock)]
pub trait HeartbeatWriter: Send + Sync {
    fn write(&self) -> Result<(), HeartbeatWriterError>;
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Heartbeat {
    written_at: std::time::SystemTime,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            written_at: std::time::SystemTime::now(),
        }
    }
}

pub struct LocalHeartbeatWriter {
    file_writer: Arc<dyn FileWriter>,
    path: PathBuf,
}

impl LocalHeartbeatWriter {
    pub fn new(file_writer: Arc<dyn FileWriter>, heartbeat_file: &Path) -> Self {
        Self {
            file_writer,
            path: heartbeat_file.to_path_buf(),
        }
    }
}

impl HeartbeatWriter for LocalHeartbeatWriter {
    fn write(&self) -> Result<(), HeartbeatWriterError> {
        let heartbeat = Heartbeat::new();
        self.file_writer
            .write_all(&self.path, &serde_json::to_string(&heartbeat)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::MockFileWriter;

    #[test]
    fn test_write_should_serialize_a_json_heartbeat_to_the_given_path() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .withf(|path, contents| {
                path == Path::new("/run/douglas/bract/heartbeat")
                    && serde_json::from_str::<serde_json::Value>(contents)
                        .is_ok_and(|value| value.get("written_at").is_some())
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let writer = LocalHeartbeatWriter::new(
            Arc::new(file_writer),
            Path::new("/run/douglas/bract/heartbeat"),
        );

        writer.write().expect("should write the heartbeat");
    }

    #[test]
    fn test_write_should_propagate_a_file_system_error() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .returning(|_, _| Err(FileSystemError::IoError(std::io::Error::other("boom"))));

        let writer = LocalHeartbeatWriter::new(
            Arc::new(file_writer),
            Path::new("/run/douglas/bract/heartbeat"),
        );

        assert!(matches!(
            writer.write(),
            Err(HeartbeatWriterError::FileSystemError(_))
        ));
    }
}
