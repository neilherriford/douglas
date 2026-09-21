use crate::util::read_if_present;
use ::config::DouglasFolders;
use file_system::{FileDeleter, FileReader, FileSystemError, FileWriter};
use os::Os;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpgradeJournal {
    pub from: String,
    pub to: String,
    pub previous_binary: PathBuf,
    pub pid: u32,
}

#[derive(Error, Debug)]
pub(crate) enum JournalError {
    #[error("The upgrade journal {} could not be serialized: {source}", .path.display())]
    Serialize {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("The upgrade journal {} could not be read: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        source: FileSystemError,
    },
    #[error("The upgrade journal {} is not valid: {source}", .path.display())]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("The upgrade journal {} could not be written: {source}", .path.display())]
    WriteFailed {
        path: PathBuf,
        source: FileSystemError,
    },
    #[error("The upgrade journal {} could not be removed: {source}", .path.display())]
    RemoveFailed {
        path: PathBuf,
        source: FileSystemError,
    },
}

pub(crate) fn record(
    douglas_folders: &DouglasFolders,
    file_writer: &dyn FileWriter,
    journal: &UpgradeJournal,
) -> Result<(), JournalError> {
    let path = douglas_folders.upgrade_journal();
    let json = serde_json::to_string(journal).map_err(|source| JournalError::Serialize {
        path: path.clone(),
        source,
    })?;

    file_writer
        .write_all(&path, &json)
        .map_err(|source| JournalError::WriteFailed { path, source })
}

pub(crate) fn load(
    douglas_folders: &DouglasFolders,
    file_reader: &dyn FileReader,
) -> Result<Option<UpgradeJournal>, JournalError> {
    let path = douglas_folders.upgrade_journal();

    let Some(raw) =
        read_if_present(file_reader, &path).map_err(|source| JournalError::Unreadable {
            path: path.clone(),
            source,
        })?
    else {
        return Ok(None);
    };

    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|source| JournalError::Invalid { path, source })
}

pub(crate) fn is_interrupted(os: &dyn Os, journal: &UpgradeJournal) -> bool {
    matches!(os.is_active_pid(journal.pid), Ok(false))
}

pub(crate) fn describe_interrupted(journal: &UpgradeJournal) -> String {
    format!(
        "An upgrade from {} to {} did not finish (the process running it is gone). The previous \
         version is kept at {}.",
        journal.from,
        journal.to,
        journal.previous_binary.display()
    )
}

pub(crate) fn clear(
    douglas_folders: &DouglasFolders,
    file_deleter: &dyn FileDeleter,
) -> Result<(), JournalError> {
    let path = douglas_folders.upgrade_journal();

    match file_deleter.delete(&path) {
        Err(source) if !source.is_not_found() => Err(JournalError::RemoveFailed { path, source }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileDeleter, MockFileReader, MockFileWriter};
    use os::MockOs;

    fn journal() -> UpgradeJournal {
        UpgradeJournal {
            from: "0.0.1".to_string(),
            to: "0.0.2".to_string(),
            previous_binary: PathBuf::from("/var/lib/douglas/bin/douglas-0.0.1"),
            pid: 4242,
        }
    }

    #[test]
    fn test_record_should_write_the_journal_to_its_path_as_json() {
        let expected_path = DouglasFolders::new().upgrade_journal();
        let mut writer = MockFileWriter::new();
        writer
            .expect_write_all()
            .withf(move |path, contents| {
                path == expected_path
                    && serde_json::from_str::<UpgradeJournal>(contents).ok() == Some(journal())
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let result = record(&DouglasFolders::new(), &writer, &journal());

        assert!(result.is_ok());
    }

    #[test]
    fn test_record_should_report_a_failed_write_with_the_path() {
        let mut writer = MockFileWriter::new();
        writer.expect_write_all().returning(|path, _| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })
        });

        let result = record(&DouglasFolders::new(), &writer, &journal());

        let Err(err) = result else {
            panic!("should fail");
        };
        assert!(matches!(err, JournalError::WriteFailed { .. }));
        assert!(err.to_string().contains("upgrade-journal.json"));
    }

    #[test]
    fn test_clear_should_delete_the_journal_at_its_path() {
        let expected_path = DouglasFolders::new().upgrade_journal();
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .withf(move |path| path == expected_path)
            .times(1)
            .returning(|_| Ok(()));

        let result = clear(&DouglasFolders::new(), &deleter);

        assert!(result.is_ok());
    }

    #[test]
    fn test_clear_should_succeed_when_there_is_no_journal() {
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));

        let result = clear(&DouglasFolders::new(), &deleter);

        assert!(result.is_ok());
    }

    #[test]
    fn test_clear_should_report_a_delete_that_fails_for_another_reason() {
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })
        });

        let result = clear(&DouglasFolders::new(), &deleter);

        assert!(matches!(result, Err(JournalError::RemoveFailed { .. })));
    }

    fn journal_json() -> String {
        let Ok(json) = serde_json::to_string(&journal()) else {
            panic!("should serialize");
        };
        json
    }

    #[test]
    fn test_load_should_read_the_journal_from_its_path() {
        let expected_path = DouglasFolders::new().upgrade_journal();
        let mut reader = MockFileReader::new();
        reader
            .expect_read_all()
            .withf(move |path| path == expected_path)
            .returning(|_| Ok(journal_json()));

        let result = load(&DouglasFolders::new(), &reader);

        assert!(matches!(result, Ok(Some(found)) if found == journal()));
    }

    #[test]
    fn test_load_should_find_nothing_when_there_is_no_journal() {
        let mut reader = MockFileReader::new();
        reader
            .expect_read_all()
            .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));

        let result = load(&DouglasFolders::new(), &reader);

        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_load_should_report_a_journal_that_cannot_be_read() {
        let mut reader = MockFileReader::new();
        reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })
        });

        let result = load(&DouglasFolders::new(), &reader);

        assert!(matches!(result, Err(JournalError::Unreadable { .. })));
    }

    #[test]
    fn test_load_should_report_a_journal_that_is_not_valid() {
        let mut reader = MockFileReader::new();
        reader
            .expect_read_all()
            .returning(|_| Ok("not json".to_string()));

        let result = load(&DouglasFolders::new(), &reader);

        assert!(matches!(result, Err(JournalError::Invalid { .. })));
    }

    #[test]
    fn test_is_interrupted_should_be_true_when_the_upgrading_process_is_gone() {
        let mut os = MockOs::new();
        os.expect_is_active_pid()
            .withf(|pid| *pid == 4242)
            .returning(|_| Ok(false));

        assert!(is_interrupted(&os, &journal()));
    }

    #[test]
    fn test_is_interrupted_should_be_false_while_the_upgrading_process_is_running() {
        let mut os = MockOs::new();
        os.expect_is_active_pid().returning(|_| Ok(true));

        assert!(!is_interrupted(&os, &journal()));
    }

    #[test]
    fn test_is_interrupted_should_be_false_when_the_process_cannot_be_checked() {
        let mut os = MockOs::new();
        os.expect_is_active_pid()
            .returning(|_| Err(os::OsError::PidTooLarge));

        assert!(!is_interrupted(&os, &journal()));
    }

    #[test]
    fn test_describe_interrupted_should_name_both_versions_and_the_kept_binary() {
        let message = describe_interrupted(&journal());

        assert!(message.contains("0.0.1"));
        assert!(message.contains("0.0.2"));
        assert!(message.contains("/var/lib/douglas/bin/douglas-0.0.1"));
    }
}
