use ::config::DouglasFolders;
use file_system::{FileDeleter, FileSystemError, FileWriter};
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
    use file_system::{MockFileDeleter, MockFileWriter};

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
}
