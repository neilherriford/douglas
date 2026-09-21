use crate::bootstrap::retention::{self, RollbackTargetError};
use crate::bootstrap::staged_copy::copy_keeping_owner;
use crate::verify::Version;
use ::config::DouglasFolders;
use file_system::{FileCopier, FileDeleter, FileSystemError, Folder, Permissions};
use std::path::PathBuf;
use thiserror::Error;

const STAGING_NAME: &str = "douglas-rollback.staging";

#[derive(Error, Debug)]
pub(crate) enum RollbackError {
    #[error("'{0}' is not a version; expected something like 0.0.1")]
    NotAVersion(String),
    #[error("The kept versions could not be listed: {0}")]
    Listing(FileSystemError),
    #[error("{0}")]
    Target(#[from] RollbackTargetError),
    #[error("The kept version could not be prepared for the rollback: {0}")]
    Staging(FileSystemError),
}

pub(crate) fn choose(
    douglas_folders: &DouglasFolders,
    folder: &dyn Folder,
    current: Version,
    requested: Option<&str>,
) -> Result<Version, RollbackError> {
    let requested = requested
        .map(|text| {
            retention::parse_version(text)
                .ok_or_else(|| RollbackError::NotAVersion(text.to_string()))
        })
        .transpose()?;

    let names: Vec<String> = folder
        .entries(&douglas_folders.binary_dir())
        .map_err(RollbackError::Listing)?
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    Ok(retention::rollback_target(&names, current, requested)?)
}

pub(crate) fn staging_path(douglas_folders: &DouglasFolders) -> PathBuf {
    douglas_folders.binary_dir().join(STAGING_NAME)
}

pub(crate) fn stage(
    douglas_folders: &DouglasFolders,
    copier: &dyn FileCopier,
    permissions: &dyn Permissions,
    deleter: &dyn FileDeleter,
    version: Version,
) -> Result<PathBuf, RollbackError> {
    let kept = retention::retained_path(&douglas_folders.binary_dir(), version);
    let staging = staging_path(douglas_folders);

    copy_keeping_owner(copier, permissions, deleter, &kept, &staging)
        .map(|()| staging)
        .map_err(RollbackError::Staging)
}

pub(crate) fn unstage(deleter: &dyn FileDeleter, staging: &std::path::Path) {
    let _ = deleter.delete(staging);
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{
        Entry, EntryKind, MockFileCopier, MockFileDeleter, MockFolder, MockPermissions,
    };
    use std::path::Path;

    fn version(major: u8, minor: u8, patch: u8) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn folder_with(names: &[&str]) -> MockFolder {
        let entries: Vec<Entry> = names
            .iter()
            .map(|name| Entry {
                name: (*name).to_string(),
                path: DouglasFolders::new().binary_dir().join(name),
                kind: EntryKind::File,
                is_link: false,
                size: 1,
            })
            .collect();
        let mut folder = MockFolder::new();
        let expected_dir = DouglasFolders::new().binary_dir();
        folder
            .expect_entries()
            .withf(move |path| path == expected_dir)
            .returning(move |_| Ok(entries.clone()));
        folder
    }

    fn permission_denied() -> FileSystemError {
        FileSystemError::IoErrorAtPath {
            path: PathBuf::from("/x"),
            error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }
    }

    #[test]
    fn test_choose_should_pick_the_newest_older_kept_version_by_default() {
        let folder = folder_with(&["douglas", "douglas-0.0.1", "douglas-0.0.2"]);

        let result = choose(&DouglasFolders::new(), &folder, version(0, 0, 3), None);

        assert!(matches!(result, Ok(found) if found == version(0, 0, 2)));
    }

    #[test]
    fn test_choose_should_honour_a_requested_version() {
        let folder = folder_with(&["douglas-0.0.1", "douglas-0.0.2"]);

        let result = choose(
            &DouglasFolders::new(),
            &folder,
            version(0, 0, 3),
            Some("0.0.1"),
        );

        assert!(matches!(result, Ok(found) if found == version(0, 0, 1)));
    }

    #[test]
    fn test_choose_should_reject_text_that_is_not_a_version() {
        let folder = MockFolder::new();

        let result = choose(
            &DouglasFolders::new(),
            &folder,
            version(0, 0, 3),
            Some("latest"),
        );

        assert!(matches!(result, Err(RollbackError::NotAVersion(text)) if text == "latest"));
    }

    #[test]
    fn test_choose_should_report_when_nothing_older_is_kept() {
        let folder = folder_with(&["douglas"]);

        let result = choose(&DouglasFolders::new(), &folder, version(0, 0, 3), None);

        assert!(matches!(
            result,
            Err(RollbackError::Target(
                RollbackTargetError::NothingToRollBackTo
            ))
        ));
    }

    #[test]
    fn test_choose_should_report_a_directory_that_cannot_be_listed() {
        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));

        let result = choose(&DouglasFolders::new(), &folder, version(0, 0, 3), None);

        assert!(matches!(result, Err(RollbackError::Listing(_))));
    }

    #[test]
    fn test_staging_path_should_sit_in_the_binary_directory_under_a_name_that_is_not_a_kept_version()
     {
        let path = staging_path(&DouglasFolders::new());

        assert_eq!(
            path.parent(),
            Some(DouglasFolders::new().binary_dir().as_path())
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert_eq!(retention::parse_retained(name), None);
    }

    #[test]
    fn test_stage_should_copy_the_kept_version_and_give_the_copy_its_owner() {
        let kept = DouglasFolders::new().binary_dir().join("douglas-0.0.1");
        let staging = staging_path(&DouglasFolders::new());
        let expected_kept = kept.clone();
        let expected_staging = staging.clone();
        let mut copier = MockFileCopier::new();
        copier
            .expect_copy()
            .withf(move |from, to| from == expected_kept && to == expected_staging)
            .times(1)
            .returning(|_, _| Ok(()));
        let owner_source = kept.clone();
        let mut permissions = MockPermissions::new();
        permissions
            .expect_get_user_and_group_ownership()
            .withf(move |path| path == owner_source)
            .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
        let owned = staging.clone();
        permissions
            .expect_change_user_and_group_ownership()
            .withf(move |path, user, group| {
                path == owned && user == "root" && group == "douglas-admin"
            })
            .times(1)
            .returning(|_, _, _| Ok(()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(0);

        let result = stage(
            &DouglasFolders::new(),
            &copier,
            &permissions,
            &deleter,
            version(0, 0, 1),
        );

        assert!(matches!(result, Ok(path) if path == staging));
    }

    #[test]
    fn test_stage_should_remove_the_staging_file_and_fail_when_the_copy_fails() {
        let staging = staging_path(&DouglasFolders::new());
        let mut copier = MockFileCopier::new();
        copier
            .expect_copy()
            .returning(|_, _| Err(permission_denied()));
        let permissions = MockPermissions::new();
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .withf(move |path| path == staging)
            .times(1)
            .returning(|_| Ok(()));

        let result = stage(
            &DouglasFolders::new(),
            &copier,
            &permissions,
            &deleter,
            version(0, 0, 1),
        );

        assert!(matches!(result, Err(RollbackError::Staging(_))));
    }

    #[test]
    fn test_stage_should_remove_the_staging_file_and_fail_when_ownership_cannot_be_set() {
        let mut copier = MockFileCopier::new();
        copier.expect_copy().returning(|_, _| Ok(()));
        let mut permissions = MockPermissions::new();
        permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Err(permission_denied()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(1).returning(|_| Ok(()));

        let result = stage(
            &DouglasFolders::new(),
            &copier,
            &permissions,
            &deleter,
            version(0, 0, 1),
        );

        assert!(matches!(result, Err(RollbackError::Staging(_))));
    }

    #[test]
    fn test_unstage_should_delete_the_staging_file_and_ignore_a_failure() {
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .withf(|path| path == Path::new("/x/douglas-rollback.staging"))
            .times(1)
            .returning(|_| Err(permission_denied()));

        unstage(&deleter, Path::new("/x/douglas-rollback.staging"));
    }
}
