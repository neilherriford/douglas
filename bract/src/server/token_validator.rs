use super::Response;
use file_system::FileReader;
use log::{ScopeKind, ScopedReporter, Span};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use utils::ClientErrorDisplay;

pub(super) struct TokenValidator {
    path: PathBuf,
    file_reader: Arc<dyn FileReader>,
}

impl TokenValidator {
    pub fn new(file_reader: Arc<dyn FileReader>, path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            file_reader,
        }
    }

    pub fn perform_if_valid<F>(&self, span: &Span, token: String, perform: F) -> Response
    where
        F: FnOnce() -> Response,
    {
        let label = "Verifying token";
        let child_span = span.create_child(label, ScopeKind::Task);
        let log = ScopedReporter::new(child_span.reporter.as_ref(), child_span.id, label);

        let expected = match self.file_reader.read_all(self.path.as_path()) {
            Ok(result) => result,
            Err(err) => {
                log.message(log::Level::Warn, &err.to_string());
                log.finish(log::Outcome::Failed);
                return Response::Error(err.to_client_string());
            }
        };
        let valid = token.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1;

        if valid {
            log.finish(log::Outcome::Ok);
            perform()
        } else {
            log.message(log::Level::Warn, "Invalid token");
            log.finish(log::Outcome::Failed);
            Response::InvalidToken
        }
    }
}
