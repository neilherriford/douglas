use crate::commands::seedling_command_context;
use crate::util::require;
use crate::verify::{BinaryVerifier, DouglasBinaryVerifier};
use file_system::{FileReader, UnixFileReader};
use os::{Os, Unix};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

pub(crate) fn verify(path: Option<PathBuf>) -> ExitCode {
    let (_, guard) = seedling_command_context("Verifying binary");

    let path = if let Some(path) = path {
        path
    } else {
        let os = Unix::new();
        let Some(path) = require(
            &guard,
            "Failed to determine current executable",
            os.current_executable(),
        ) else {
            return ExitCode::from(1);
        };
        path
    };

    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let verify_binary = DouglasBinaryVerifier::new(file_reader);

    match verify_binary.get_external_version(&path) {
        Ok(version) => {
            guard.span().message(
                log::Level::Info,
                &format!(
                    "{} is signed correctly (v{}.{}.{})",
                    path.display(),
                    version.major,
                    version.minor,
                    version.patch
                ),
            );
            ExitCode::from(0)
        }
        Err(err) => {
            guard.span().message(log::Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            ExitCode::from(1)
        }
    }
}
