use crate::directory::Directory;
use crate::os::OsError;
use file_system::{FileReader, FileSystemError};
use futures::{SinkExt, StreamExt};
use log::Logger;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::net::UnixStream;

use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", content = "data")]
pub enum Response {
    CreatedDirectoryEntry { user: String, group: String },
    InvalidToken,
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Request {
    CreateDirectoryEntry { token: String, service_name: String },
}

#[derive(Error, Debug)]
pub enum RequestHandlerError {
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("OS error {0}")]
    OsError(#[from] OsError),
}

#[async_trait::async_trait]
pub trait RequestHandler {
    async fn handle(&self, socket: UnixStream) -> Result<(), RequestHandlerError>;
}

pub struct LocalRequestHandler {
    log: Arc<dyn Logger + 'static>,
    file_reader: Box<dyn FileReader + Sync + Send + 'static>,
    directory: Arc<dyn Directory + Send + Sync + 'static>,
    token_path: PathBuf,
}

impl LocalRequestHandler {
    pub fn new(
        log: Arc<dyn Logger + 'static>,
        file_reader: impl FileReader + Sync + Send + 'static,
        directory: Arc<dyn Directory + Send + Sync + 'static>,
        token_path: &Path,
    ) -> Self {
        Self {
            log,
            file_reader: Box::new(file_reader),
            directory,
            token_path: token_path.to_path_buf(),
        }
    }

    fn create_directory_entry(
        &self,
        token: String,
        service_name: String,
    ) -> Result<Response, RequestHandlerError> {
        self.log.info("Verifying token");
        if self.validate_token(token)? {
            self.log.info("Creating directory entries");
            let mut service_name = service_name.to_string();
            service_name.truncate(26);

            let name = format!("doug-{}", service_name).to_string();
            self.directory.create_group(&name)?;
            self.directory
                .create_user(&name, &name, vec!["douglas".to_string()])?;

            Ok(Response::CreatedDirectoryEntry {
                user: name.clone(),
                group: name,
            })
        } else {
            self.log.warn("Invalid token");
            Ok(Response::InvalidToken)
        }
    }

    fn validate_token(&self, token: String) -> Result<bool, FileSystemError> {
        let expected = self.file_reader.read_all(self.token_path.as_path())?;
        Ok(token.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1)
    }
}

#[async_trait::async_trait]
impl RequestHandler for LocalRequestHandler {
    async fn handle(&self, socket: UnixStream) -> Result<(), RequestHandlerError> {
        let length_delimited = Framed::new(socket, LengthDelimitedCodec::new());
        let mut transport =
            SerdeFramed::new(length_delimited, Json::<Request, Response>::default());

        match transport.next().await {
            Some(Ok(request)) => {
                self.log.info(&format!("Received request{:?}", request));
                let response = match request {
                    Request::CreateDirectoryEntry {
                        token,
                        service_name,
                    } => self.create_directory_entry(token, service_name)?,
                };

                self.log.info("Completed request");
                let _ = transport.send(response).await;
            }
            None => self.log.warn("Invalid request"),
            Some(Err(err)) => self.log.error(&format!("Invalid request: {:?}", err)),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod create_directory_entry {
        use super::super::*;
        use crate::directory::MockDirectory;
        use file_system::MockFileReader;
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_error_if_token_unreadable() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());

            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry("foo".to_string(), "bar".to_string());

            assert!(matches!(
                actual,
                Err(RequestHandlerError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn should_handle_invalid_token() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            log.expect_warn()
                .with(predicate::eq("Invalid token"))
                .returning(|_| ());

            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("not_foo".to_string()));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry("foo".to_string(), "bar".to_string());

            assert!(matches!(actual, Ok(Response::InvalidToken)));
        }

        #[test]
        fn should_return_error_if_group_creation_failed() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            log.expect_warn()
                .with(predicate::eq("Invalid token"))
                .returning(|_| ());

            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("foo".to_string()));

            directory
                .expect_create_group()
                .with(predicate::eq("doug-bar"))
                .returning(|_| Err(OsError::InvalidName));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry("foo".to_string(), "bar".to_string());

            assert!(matches!(
                actual,
                Err(RequestHandlerError::OsError(OsError::InvalidName))
            ));
        }

        #[test]
        fn should_error_if_user_creation_fails() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("foo".to_string()));

            directory
                .expect_create_group()
                .with(predicate::eq("doug-bar".to_string()))
                .returning(|_| Ok(()));

            directory
                .expect_create_user()
                .with(
                    predicate::eq("doug-bar"),
                    predicate::eq("doug-bar"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Err(OsError::InvalidName));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry("foo".to_string(), "bar".to_string());

            assert!(matches!(
                actual,
                Err(RequestHandlerError::OsError(OsError::InvalidName))
            ));
        }

        #[test]
        fn should_create_entries() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("foo".to_string()));

            directory
                .expect_create_group()
                .with(predicate::eq("doug-bar"))
                .returning(|_| Ok(()));

            directory
                .expect_create_user()
                .with(
                    predicate::eq("doug-bar"),
                    predicate::eq("doug-bar"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Ok(()));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry("foo".to_string(), "bar".to_string());

            assert!(matches!(
                actual,
                Ok(Response::CreatedDirectoryEntry { user, group })
                if user == "doug-bar".to_string() && group == "doug-bar".to_string(),
            ));
        }

        #[test]
        fn should_truncate_long_service_names() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut directory = MockDirectory::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("foo".to_string()));

            directory
                .expect_create_group()
                .with(predicate::eq("doug-lorem_ipsum_dolor_sit_amet"))
                .returning(|_| Ok(()));

            directory
                .expect_create_user()
                .with(
                    predicate::eq("doug-lorem_ipsum_dolor_sit_amet"),
                    predicate::eq("doug-lorem_ipsum_dolor_sit_amet"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Ok(()));

            let actual = LocalRequestHandler::new(
                Arc::new(log),
                file_reader,
                Arc::new(directory),
                token_path,
            )
            .create_directory_entry(
                "foo".to_string(),
                "lorem_ipsum_dolor_sit_amet_consectetur_adipiscing_elit".to_string(),
            );

            assert!(matches!(
                actual,
                Ok(Response::CreatedDirectoryEntry { user, group })
                if user == "doug-lorem_ipsum_dolor_sit_amet".to_string() && group == "doug-lorem_ipsum_dolor_sit_amet".to_string(),
            ));
        }
    }
}
