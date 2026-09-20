use crate::cli::OutputStyle;
use crate::commands::{CommandContext, print_error};
use crate::verify::{BinaryVerifier, DouglasBinaryVerifier, Release};
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
    format: u8,
    core: std::collections::BTreeMap<String, u16>,
}

fn describe(release: &Release) -> String {
    let core = if release.metadata.core.is_empty() {
        "none".to_string()
    } else {
        release
            .metadata
            .core
            .iter()
            .map(|(name, version)| format!("{name} {version}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "v{}, data format {}, core {core}",
        release.version, release.metadata.format
    )
}

fn print_verified(output_style: OutputStyle, path: &Path, release: &Release) {
    match output_style {
        OutputStyle::Plain => println!(
            "{} is signed correctly ({})",
            path.display(),
            describe(release)
        ),
        OutputStyle::Json => {
            let response = JsonVerifyResponse {
                success: true,
                path: path.display().to_string(),
                version: release.version.to_string(),
                format: release.metadata.format,
                core: release.metadata.core.clone(),
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

    let os: Arc<dyn Os> = Arc::new(os);
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let verify_binary = DouglasBinaryVerifier::new(os, file_reader);

    match verify_binary.get_external_release(&path) {
        Ok(release) => {
            guard.span().message(
                log::Level::Info,
                &format!(
                    "{} is signed correctly ({})",
                    path.display(),
                    describe(&release)
                ),
            );
            print_verified(output_style, &path, &release);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::Version;
    use release::ReleaseMetadata;
    use std::collections::BTreeMap;

    fn release_with(metadata: ReleaseMetadata) -> Release {
        Release {
            version: Version {
                major: 0,
                minor: 2,
                patch: 1,
            },
            metadata,
        }
    }

    #[test]
    fn test_describe_should_list_the_version_format_and_each_core_version() {
        let release = release_with(ReleaseMetadata {
            format: 3,
            core: BTreeMap::from([("openbao".to_string(), 2), ("traefik".to_string(), 1)]),
        });

        assert_eq!(
            describe(&release),
            "v0.2.1, data format 3, core openbao 2, traefik 1"
        );
    }

    #[test]
    fn test_describe_should_say_none_when_there_are_no_core_versions() {
        let release = release_with(ReleaseMetadata {
            format: 0,
            core: BTreeMap::new(),
        });

        assert_eq!(describe(&release), "v0.2.1, data format 0, core none");
    }
}
