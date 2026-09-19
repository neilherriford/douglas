use config::DouglasFolders;
use file_system::{FileReader, FileSystemError, FileWriter};
#[cfg(feature = "mock")]
use mockall::automock;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
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
pub struct Heartbeat {
    pub pid: u32,
    pub written_at: SystemTime,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            pid: std::process::id(),
            written_at: SystemTime::now(),
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

#[derive(Error, Debug)]
pub enum HeartbeatReaderError {
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Serialization error: {0}")]
    SupportFileSerializationError(#[from] serde_json::Error),
}

pub trait HeartbeatReader: Send + Sync {
    fn read(&self) -> Result<Heartbeat, HeartbeatReaderError>;
}

pub struct LocalHeartbeatReader {
    path: PathBuf,
    file_reader: Arc<dyn FileReader>,
}

impl LocalHeartbeatReader {
    pub fn new(heartbeat_file: &Path, file_reader: Arc<dyn FileReader>) -> Self {
        Self {
            path: heartbeat_file.to_path_buf(),
            file_reader,
        }
    }
}

impl HeartbeatReader for LocalHeartbeatReader {
    fn read(&self) -> Result<Heartbeat, HeartbeatReaderError> {
        let raw = self.file_reader.read_all(&self.path)?;
        Ok(serde_json::from_str::<Heartbeat>(&raw)?)
    }
}

#[cfg_attr(feature = "mock", automock)]
pub trait HeartbeatReaderFactory: Send + Sync {
    fn create(&self, service_name: &str) -> Box<dyn HeartbeatReader>;
}

pub struct LocalHeartbeatReaderFactory {
    douglas_folders: DouglasFolders,
    file_reader: Arc<dyn FileReader>,
}

impl LocalHeartbeatReaderFactory {
    pub fn new(douglas_folders: DouglasFolders, file_reader: Arc<dyn FileReader>) -> Self {
        Self {
            douglas_folders,
            file_reader,
        }
    }
}

impl HeartbeatReaderFactory for LocalHeartbeatReaderFactory {
    fn create(&self, service_name: &str) -> Box<dyn HeartbeatReader> {
        let heartbeat_file = self.douglas_folders.service_heartbeat_file(service_name);
        Box::new(LocalHeartbeatReader::new(
            &heartbeat_file,
            Arc::clone(&self.file_reader),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileReader, MockFileWriter};

    #[test]
    fn test_write_should_serialize_a_json_heartbeat_to_the_given_path() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .withf(|path, contents| {
                path == Path::new("/run/douglas/bract/heartbeat")
                    && serde_json::from_str::<serde_json::Value>(contents).is_ok_and(|value| {
                        value.get("written_at").is_some() && value.get("pid").is_some()
                    })
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
    fn test_read_should_recover_the_pid_and_timestamp_that_were_stored() {
        let stored = Heartbeat {
            pid: 4242,
            written_at: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(9000),
        };
        let serialized = serde_json::to_string(&stored).expect("should serialize");

        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .withf(|path| path == Path::new("/run/douglas/bract/heartbeat"))
            .returning(move |_| Ok(serialized.clone()));

        let reader = LocalHeartbeatReader::new(
            Path::new("/run/douglas/bract/heartbeat"),
            Arc::new(file_reader),
        );

        let recovered = reader.read().expect("should parse the heartbeat");
        assert_eq!(recovered.pid, 4242);
        assert_eq!(recovered.written_at, stored.written_at);
    }

    #[test]
    fn test_read_should_propagate_a_file_system_error() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .returning(|_| Err(FileSystemError::IoError(std::io::Error::other("boom"))));

        let reader = LocalHeartbeatReader::new(
            Path::new("/run/douglas/bract/heartbeat"),
            Arc::new(file_reader),
        );

        assert!(matches!(
            reader.read(),
            Err(HeartbeatReaderError::FileSystemError(_))
        ));
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
