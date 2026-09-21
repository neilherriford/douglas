use crate::util::{join_display, read_if_present};
use crate::verify::{BinaryVerifier, VerifyError};
use ::config::DouglasFolders;
use file_system::{FileReader, FileSystemError, FileWriter};
use release::{Conflict, InstallMarker, ReleaseError, ReleaseMetadata};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CompatibilityError {
    #[error("The install marker {} could not be read: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        source: FileSystemError,
    },
    #[error(
        "The install marker {} is not valid ({source}); if you are sure which release last ran here, remove it and run start again",
        .path.display()
    )]
    Invalid { path: PathBuf, source: ReleaseError },
    #[error(
        "This binary cannot start against the data installed on this host: {}",
        join_display(.0, "; ")
    )]
    Incompatible(Vec<Conflict>),
    #[error("The install marker {} could not be written: {source}", .path.display())]
    WriteFailed {
        path: PathBuf,
        source: FileSystemError,
    },
}

pub(crate) fn ensure_compatible(
    douglas_folders: &DouglasFolders,
    file_reader: &dyn FileReader,
    running: &ReleaseMetadata,
) -> Result<(), CompatibilityError> {
    let path = douglas_folders.install_marker();

    let Some(raw) =
        read_if_present(file_reader, &path).map_err(|source| CompatibilityError::Unreadable {
            path: path.clone(),
            source,
        })?
    else {
        return Ok(());
    };

    let marker = InstallMarker::from_json(&raw)
        .map_err(|source| CompatibilityError::Invalid { path, source })?;

    let conflicts = release::start_conflicts(&marker, running);
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(CompatibilityError::Incompatible(conflicts))
    }
}

pub(crate) fn running_version(
    verifier: &dyn BinaryVerifier,
    unsigned_fallback: &str,
) -> Result<String, VerifyError> {
    match verifier.get_internal_release() {
        Ok(release) => Ok(release.version.to_string()),
        Err(VerifyError::MissingTrailer(_)) => Ok(unsigned_fallback.to_string()),
        Err(err) => Err(err),
    }
}

pub(crate) fn record_install(
    douglas_folders: &DouglasFolders,
    file_writer: &dyn FileWriter,
    version: &str,
    running: &ReleaseMetadata,
) -> Result<(), CompatibilityError> {
    let path = douglas_folders.install_marker();
    let marker = InstallMarker {
        version: version.to_string(),
        metadata: running.clone(),
    };

    let json = marker
        .to_json()
        .map_err(|source| CompatibilityError::Invalid {
            path: path.clone(),
            source,
        })?;

    file_writer
        .write_all(&path, &json)
        .map_err(|source| CompatibilityError::WriteFailed { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileReader, MockFileWriter};
    use std::collections::BTreeMap;

    fn metadata(format: u8, openbao: u16) -> ReleaseMetadata {
        ReleaseMetadata {
            format,
            core: BTreeMap::from([("openbao".to_string(), openbao)]),
        }
    }

    fn marker_json(format: u8, openbao: u16) -> String {
        let marker = InstallMarker {
            version: "0.2.1".to_string(),
            metadata: metadata(format, openbao),
        };
        let Ok(json) = marker.to_json() else {
            panic!("should serialize");
        };
        json
    }

    fn reader_returning(contents: String) -> MockFileReader {
        let expected = DouglasFolders::new().install_marker();
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .withf(move |path| path == expected)
            .returning(move |_| Ok(contents.clone()));
        file_reader
    }

    fn reader_failing_with(error: fn() -> FileSystemError) -> MockFileReader {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .returning(move |_| Err(error()));
        file_reader
    }

    fn not_found() -> FileSystemError {
        FileSystemError::NotFoundError(DouglasFolders::new().install_marker())
    }

    fn io_not_found() -> FileSystemError {
        FileSystemError::IoErrorAtPath {
            path: DouglasFolders::new().install_marker(),
            error: std::io::Error::from(std::io::ErrorKind::NotFound),
        }
    }

    fn permission_denied() -> FileSystemError {
        FileSystemError::IoErrorAtPath {
            path: DouglasFolders::new().install_marker(),
            error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }
    }

    mod ensure_compatible_tests {
        use super::*;

        #[test]
        fn test_should_allow_a_fresh_host_with_no_marker() {
            let file_reader = reader_failing_with(not_found);

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(result.is_ok());
        }

        #[test]
        fn test_should_treat_an_io_not_found_error_as_a_fresh_host() {
            let file_reader = reader_failing_with(io_not_found);

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(result.is_ok());
        }

        #[test]
        fn test_should_read_the_marker_from_the_install_marker_path() {
            let file_reader = reader_returning(marker_json(1, 1));

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(result.is_ok());
        }

        #[test]
        fn test_should_allow_a_binary_newer_than_the_installed_data() {
            let file_reader = reader_returning(marker_json(1, 1));

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(2, 2));

            assert!(result.is_ok());
        }

        #[test]
        fn test_should_refuse_a_binary_older_than_the_installed_data() {
            let file_reader = reader_returning(marker_json(3, 1));

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(2, 1));

            assert!(matches!(
                result,
                Err(CompatibilityError::Incompatible(conflicts))
                    if conflicts == vec![Conflict::FormatNewer { installed: 3, running: 2 }]
            ));
        }

        #[test]
        fn test_should_refuse_when_a_core_seedling_is_newer_than_the_binary_declares() {
            let file_reader = reader_returning(marker_json(1, 2));

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(matches!(
                result,
                Err(CompatibilityError::Incompatible(conflicts)) if conflicts.len() == 1
            ));
        }

        #[test]
        fn test_should_report_an_unreadable_marker_rather_than_assume_a_fresh_host() {
            let file_reader = reader_failing_with(permission_denied);

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(matches!(result, Err(CompatibilityError::Unreadable { .. })));
        }

        #[test]
        fn test_should_refuse_a_marker_that_is_not_valid() {
            let file_reader = reader_returning("not json".to_string());

            let result = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(1, 1));

            assert!(matches!(result, Err(CompatibilityError::Invalid { .. })));
        }

        #[test]
        fn test_the_incompatible_message_should_name_the_conflict() {
            let file_reader = reader_returning(marker_json(3, 1));

            let Err(err) = ensure_compatible(&DouglasFolders::new(), &file_reader, &metadata(2, 1))
            else {
                panic!("should refuse");
            };

            assert!(err.to_string().contains("data format"));
        }
    }

    mod running_version_tests {
        use super::*;
        use crate::verify::{MockBinaryVerifier, Release, Version};

        #[test]
        fn test_should_report_the_version_from_the_signed_trailer() {
            let mut verifier = MockBinaryVerifier::new();
            verifier.expect_get_internal_release().returning(|| {
                Ok(Release {
                    version: Version {
                        major: 1,
                        minor: 4,
                        patch: 9,
                    },
                    metadata: metadata(1, 1),
                })
            });

            let result = running_version(&verifier, "0.0.1");

            assert!(matches!(result.as_deref(), Ok("1.4.9")));
        }

        #[test]
        fn test_should_fall_back_when_the_binary_is_unsigned() {
            let mut verifier = MockBinaryVerifier::new();
            verifier
                .expect_get_internal_release()
                .returning(|| Err(VerifyError::MissingTrailer(PathBuf::from("/x/douglas"))));

            let result = running_version(&verifier, "0.0.1");

            assert!(matches!(result.as_deref(), Ok("0.0.1")));
        }

        #[test]
        fn test_should_fail_when_the_release_cannot_be_verified() {
            let mut verifier = MockBinaryVerifier::new();
            verifier
                .expect_get_internal_release()
                .returning(|| Err(VerifyError::UnknownInternalVersion));

            let result = running_version(&verifier, "0.0.1");

            assert!(matches!(result, Err(VerifyError::UnknownInternalVersion)));
        }
    }

    mod record_install_tests {
        use super::*;
        use std::sync::{Arc, Mutex};

        #[test]
        fn test_should_write_the_marker_for_the_running_release_to_the_install_marker_path() {
            let written: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&written);
            let expected = DouglasFolders::new().install_marker();
            let mut file_writer = MockFileWriter::new();
            file_writer
                .expect_write_all()
                .withf(move |path, _| path == expected)
                .times(1)
                .returning(move |_, contents| {
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(contents.to_string());
                    }
                    Ok(())
                });

            let result = record_install(
                &DouglasFolders::new(),
                &file_writer,
                "0.2.1",
                &metadata(2, 3),
            );

            assert!(result.is_ok());
            let Ok(written) = written.lock() else {
                panic!("written mutex poisoned");
            };
            let Ok(marker) = InstallMarker::from_json(&written[0]) else {
                panic!("the written marker should parse");
            };
            assert_eq!(marker.version, "0.2.1");
            assert_eq!(marker.metadata, metadata(2, 3));
        }

        #[test]
        fn test_should_report_a_write_failure() {
            let mut file_writer = MockFileWriter::new();
            file_writer
                .expect_write_all()
                .returning(|_, _| Err(permission_denied()));

            let result = record_install(
                &DouglasFolders::new(),
                &file_writer,
                "0.2.1",
                &metadata(1, 1),
            );

            assert!(matches!(
                result,
                Err(CompatibilityError::WriteFailed { .. })
            ));
        }
    }
}
