use file_system::{FileCopier, FileDeleter, FileRenamer, FileSystemError, Permissions};
use std::path::Path;

pub(crate) fn copy_keeping_owner(
    copier: &dyn FileCopier,
    permissions: &dyn Permissions,
    deleter: &dyn FileDeleter,
    from: &Path,
    to: &Path,
) -> Result<(), FileSystemError> {
    let result = copier.copy(from, to).and_then(|()| {
        let (user, group) = permissions.get_user_and_group_ownership(from)?;
        permissions.change_user_and_group_ownership(to, &user, &group)
    });

    if result.is_err() {
        let _ = deleter.delete(to);
    }
    result
}

pub(crate) struct Installer<'a> {
    pub copier: &'a dyn FileCopier,
    pub permissions: &'a dyn Permissions,
    pub renamer: &'a dyn FileRenamer,
    pub deleter: &'a dyn FileDeleter,
}

impl Installer<'_> {
    pub(crate) fn install_copy(
        &self,
        from: &Path,
        staging: &Path,
        target: &Path,
    ) -> Result<(), FileSystemError> {
        copy_keeping_owner(self.copier, self.permissions, self.deleter, from, staging)?;

        let result = self.renamer.rename(staging, target);
        if result.is_err() {
            let _ = self.deleter.delete(staging);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileCopier, MockFileDeleter, MockFileRenamer, MockPermissions};
    use mockall::Sequence;
    use std::path::PathBuf;

    fn permission_denied() -> FileSystemError {
        FileSystemError::IoErrorAtPath {
            path: PathBuf::from("/x"),
            error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }
    }

    fn owner_of_source() -> MockPermissions {
        let mut permissions = MockPermissions::new();
        permissions
            .expect_get_user_and_group_ownership()
            .withf(|path| path == Path::new("/from"))
            .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
        permissions
    }

    #[test]
    fn test_copy_keeping_owner_should_copy_then_give_the_copy_the_owner_of_the_source() {
        let mut sequence = Sequence::new();
        let mut copier = MockFileCopier::new();
        copier
            .expect_copy()
            .withf(|from, to| from == Path::new("/from") && to == Path::new("/to"))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _| Ok(()));
        let mut permissions = owner_of_source();
        permissions
            .expect_change_user_and_group_ownership()
            .withf(|path, user, group| {
                path == Path::new("/to") && user == "root" && group == "douglas-admin"
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_, _, _| Ok(()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(0);

        let result = copy_keeping_owner(
            &copier,
            &permissions,
            &deleter,
            Path::new("/from"),
            Path::new("/to"),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_copy_keeping_owner_should_remove_the_copy_and_fail_when_the_copy_fails() {
        let mut copier = MockFileCopier::new();
        copier
            .expect_copy()
            .returning(|_, _| Err(permission_denied()));
        let permissions = MockPermissions::new();
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .withf(|path| path == Path::new("/to"))
            .times(1)
            .returning(|_| Ok(()));

        let result = copy_keeping_owner(
            &copier,
            &permissions,
            &deleter,
            Path::new("/from"),
            Path::new("/to"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_copy_keeping_owner_should_remove_the_copy_and_fail_when_the_owner_cannot_be_read() {
        let mut copier = MockFileCopier::new();
        copier.expect_copy().returning(|_, _| Ok(()));
        let mut permissions = MockPermissions::new();
        permissions
            .expect_get_user_and_group_ownership()
            .returning(|_| Err(permission_denied()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(1).returning(|_| Ok(()));

        let result = copy_keeping_owner(
            &copier,
            &permissions,
            &deleter,
            Path::new("/from"),
            Path::new("/to"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_copy_keeping_owner_should_remove_the_copy_and_fail_when_the_owner_cannot_be_set() {
        let mut copier = MockFileCopier::new();
        copier.expect_copy().returning(|_, _| Ok(()));
        let mut permissions = owner_of_source();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Err(permission_denied()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(1).returning(|_| Ok(()));

        let result = copy_keeping_owner(
            &copier,
            &permissions,
            &deleter,
            Path::new("/from"),
            Path::new("/to"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_install_copy_should_rename_the_staged_copy_onto_the_target() {
        let mut copier = MockFileCopier::new();
        copier.expect_copy().returning(|_, _| Ok(()));
        let mut permissions = owner_of_source();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Ok(()));
        let mut renamer = MockFileRenamer::new();
        renamer
            .expect_rename()
            .withf(|from, to| from == Path::new("/staging") && to == Path::new("/target"))
            .times(1)
            .returning(|_, _| Ok(()));
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(0);
        let installer = Installer {
            copier: &copier,
            permissions: &permissions,
            renamer: &renamer,
            deleter: &deleter,
        };

        let result = installer.install_copy(
            Path::new("/from"),
            Path::new("/staging"),
            Path::new("/target"),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_install_copy_should_remove_the_staged_copy_and_fail_when_the_rename_fails() {
        let mut copier = MockFileCopier::new();
        copier.expect_copy().returning(|_, _| Ok(()));
        let mut permissions = owner_of_source();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Ok(()));
        let mut renamer = MockFileRenamer::new();
        renamer
            .expect_rename()
            .returning(|_, _| Err(permission_denied()));
        let mut deleter = MockFileDeleter::new();
        deleter
            .expect_delete()
            .withf(|path| path == Path::new("/staging"))
            .times(1)
            .returning(|_| Ok(()));
        let installer = Installer {
            copier: &copier,
            permissions: &permissions,
            renamer: &renamer,
            deleter: &deleter,
        };

        let result = installer.install_copy(
            Path::new("/from"),
            Path::new("/staging"),
            Path::new("/target"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_install_copy_should_not_rename_when_staging_the_copy_fails() {
        let mut copier = MockFileCopier::new();
        copier
            .expect_copy()
            .returning(|_, _| Err(permission_denied()));
        let permissions = MockPermissions::new();
        let mut renamer = MockFileRenamer::new();
        renamer.expect_rename().times(0);
        let mut deleter = MockFileDeleter::new();
        deleter.expect_delete().times(1).returning(|_| Ok(()));
        let installer = Installer {
            copier: &copier,
            permissions: &permissions,
            renamer: &renamer,
            deleter: &deleter,
        };

        let result = installer.install_copy(
            Path::new("/from"),
            Path::new("/staging"),
            Path::new("/target"),
        );

        assert!(result.is_err());
    }
}
