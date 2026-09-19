use crate::cli::OutputStyle;
use crate::commands::{CommandContext, print_error};
use crate::verify::{BinaryVerifier, DouglasBinaryVerifier, Version};
use file_system::{FileReader, UnixFileReader};
use os::{Os, Unix};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(serde::Serialize)]
struct JsonVerifyResponse {
    success: bool,
    path: String,
    version: String,
}

fn print_verified(output_style: OutputStyle, path: &Path, version: Version) {
    match output_style {
        OutputStyle::Plain => println!("{} is signed correctly (v{version})", path.display()),
        OutputStyle::Json => {
            let response = JsonVerifyResponse {
                success: true,
                path: path.display().to_string(),
                version: version.to_string(),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                println!("{json}");
            }
        }
    }
}

pub(crate) fn verify(path: Option<PathBuf>, output_style: OutputStyle) -> ExitCode {
    let context = CommandContext::plain();
    let guard = context.task("Verifying binary");

    let os = Unix::new();
    let path = match path {
        Some(path) => path,
        None => match os.current_executable() {
            Ok(path) => path,
            Err(err) => {
                let message = format!("Failed to determine current executable: {err}");
                guard.span().message(log::Level::Warn, &message);
                print_error(output_style, &message);
                guard.finish_with_outcome(log::Outcome::Failed);
                return ExitCode::from(1);
            }
        },
    };

    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let verify_binary = DouglasBinaryVerifier::new(file_reader);

    match verify_binary.get_external_version(&path) {
        Ok(version) => {
            guard.span().message(
                log::Level::Info,
                &format!("{} is signed correctly (v{version})", path.display()),
            );
            print_verified(output_style, &path, version);
            ExitCode::from(0)
        }
        Err(err) => {
            guard.span().message(log::Level::Warn, &err.to_string());
            print_error(output_style, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            ExitCode::from(1)
        }
    }
}
