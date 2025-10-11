use super::{ClientErrorDisplay, Response};
use file_system::FileReader;
use log::Logger;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub(super) struct TokenValidator {
    path: PathBuf,
    log: Arc<dyn Logger + Sync + Send>,
    file_reader: Arc<dyn FileReader + Sync + Send>,
}

impl TokenValidator {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        file_reader: Arc<dyn FileReader + Sync + Send>,
        path: &Path,
    ) -> Self {
        Self {
            log,
            path: path.to_path_buf(),
            file_reader,
        }
    }

    pub fn perform_if_valid<F>(&self, token: String, perform: F) -> Response
    where
        F: FnOnce() -> Response,
    {
        self.log.info("Verifying token…");

        let expected = or_log_and_return_error!(
            self.log => warn,
            self.file_reader.read_all(self.path.as_path())
        );
        let valid = token.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1;

        if valid {
            self.log.info("Verified!");
            perform()
        } else {
            self.log.warn("Invalid token");
            Response::InvalidToken
        }
    }
}

#[cfg(test)]
mod tests {
    mod token_validator {
        use crate::server::Response;

        use super::super::TokenValidator;
        use file_system::{FileSystemError, MockFileReader};
        use log::MockLogger;
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_return_error_if_token_not_readable() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            log.expect_warn().returning(|_| ());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let mut called = false;
            let actual = TokenValidator::new(Arc::new(log), Arc::new(file_reader), token_path)
                .perform_if_valid("foo".to_string(), || {
                    called = true;
                    Response::Error("foo".to_string())
                });

            assert!(matches!(actual, Response::Error(_)));
        }

        #[test]
        fn should_return_invalid_token() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            log.expect_warn().returning(|_| ());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");

            let mut called = false;
            let actual = TokenValidator::new(Arc::new(log), Arc::new(file_reader), token_path)
                .perform_if_valid("foo".to_string(), || {
                    called = true;
                    Response::Error("foo".to_string())
                });

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_perform() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            log.expect_info().returning(|_| ());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");

            let mut called = false;
            let actual = TokenValidator::new(Arc::new(log), Arc::new(file_reader), token_path)
                .perform_if_valid("token".to_string(), || {
                    called = true;
                    Response::Error("foo".to_string())
                });

            assert!(called);
            assert!(matches!(actual, Response::Error(msg) if msg == "foo"));
        }
    }
}
