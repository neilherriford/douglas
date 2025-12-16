use config::constants::DOUGLAS_ADMIN_GROUP;
use file_system::{FileWriter, Modes, Permissions};
use log::Logger;
use os::Os;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub(super) struct TokenRefresher {
    log: Arc<dyn Logger + Sync + Send>,
    token_path: PathBuf,
    permissions: Arc<dyn Permissions>,
    file_writer: Arc<dyn FileWriter>,
    os: Arc<dyn Os>,
}

impl TokenRefresher {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        token_path: &Path,
        permissions: Arc<dyn Permissions>,
        file_writer: Arc<dyn FileWriter>,
        os: Arc<dyn Os>,
    ) -> Self {
        Self {
            log,
            token_path: token_path.to_path_buf(),
            permissions,
            file_writer,
            os,
        }
    }

    pub fn refresh(&self) {
        self.log.info("Refrehing token");

        let token = Uuid::now_v7().to_string();
        self.assert_ok(self.file_writer.write_all(&self.token_path.clone(), &token));

        self.assert_ok(self.permissions.change_user_and_group_ownership(
            &self.token_path,
            credentials::ROOT_USER_NAME,
            DOUGLAS_ADMIN_GROUP,
        ));

        self.assert_ok(
            self.permissions
                .change_mode(&self.token_path, &Modes::OwnerReadWriteGroupRead),
        );

        self.log.info("Token refreshed");
    }

    fn assert_ok<R, E>(&self, result: Result<R, E>)
    where
        E: Debug,
    {
        match result {
            Ok(_) => (),
            Err(err) => {
                self.log
                    .error(&format!("Token refresh error: {:?}!  Exiting!", err));
                self.os.exit(-1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod token_refresher {
        use file_system::{FileSystemError, MockFileWriter, MockPermissions, Modes};
        use log::MockLogger;
        use mockall::predicate;
        use os::MockOs;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        use std::{path::Path, sync::Arc};

        use super::super::TokenRefresher;

        #[test]
        fn should_exit_if_write_fails() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();
            let mut os = MockOs::new();

            log.expect_info().return_const(());
            log.expect_error().return_const(());
            file_writer
                .expect_write_all()
                .with(predicate::eq(Path::new("/tmp/token")), predicate::always())
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));
            os.expect_exit_with(-1);

            let refresher = TokenRefresher::new(
                Arc::new(log),
                token_path,
                Arc::new(permissions),
                Arc::new(file_writer),
                Arc::new(os),
            );
            let result = catch_unwind(AssertUnwindSafe(|| {
                refresher.refresh();
            }));

            assert!(result.is_err());
            assert!(result.unwrap_err().downcast_ref::<&'static str>() == Some(&"mock exit"));
        }

        #[test]
        fn should_exit_if_permissions_ownership_fails() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();
            let mut os = MockOs::new();

            log.expect_info().return_const(());
            log.expect_error().return_const(());
            file_writer
                .expect_write_all()
                .with(predicate::eq(Path::new("/tmp/token")), predicate::always())
                .returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq("root"),
                    predicate::eq("douglas-admin"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));
            os.expect_exit_with(-1);

            let refresher = TokenRefresher::new(
                Arc::new(log),
                token_path,
                Arc::new(permissions),
                Arc::new(file_writer),
                Arc::new(os),
            );
            let result = catch_unwind(AssertUnwindSafe(|| {
                refresher.refresh();
            }));

            assert!(result.is_err());
            assert!(result.unwrap_err().downcast_ref::<&'static str>() == Some(&"mock exit"));
        }

        #[test]
        fn should_exit_if_permissions_mode_fails() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();
            let mut os = MockOs::new();

            log.expect_info().return_const(());
            log.expect_error().return_const(());
            file_writer
                .expect_write_all()
                .with(predicate::eq(Path::new("/tmp/token")), predicate::always())
                .returning(|_, _| Ok(()));
            permissions.expect_ownership_to_be_set("/tmp/token", "root", "douglas-admin");
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/token")),
                    predicate::eq(Modes::OwnerReadWriteGroupRead),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));
            os.expect_exit_with(-1);

            let refresher = TokenRefresher::new(
                Arc::new(log),
                token_path,
                Arc::new(permissions),
                Arc::new(file_writer),
                Arc::new(os),
            );
            let result = catch_unwind(AssertUnwindSafe(|| {
                refresher.refresh();
            }));

            assert!(result.is_err());
            assert!(result.unwrap_err().downcast_ref::<&'static str>() == Some(&"mock exit"));
        }

        #[test]
        fn should_refresh_token() {
            let mut log = MockLogger::new();
            let token_path = Path::new("/tmp/token");
            let mut permissions = MockPermissions::new();
            let mut file_writer = MockFileWriter::new();
            let os = MockOs::new();

            log.expect_info().return_const(());
            log.expect_error().return_const(());
            file_writer
                .expect_write_all()
                .with(predicate::eq(Path::new("/tmp/token")), predicate::always())
                .returning(|_, _| Ok(()));
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/token",
                "root",
                "douglas-admin",
                Modes::OwnerReadWriteGroupRead,
            );

            TokenRefresher::new(
                Arc::new(log),
                token_path,
                Arc::new(permissions),
                Arc::new(file_writer),
                Arc::new(os),
            )
            .refresh();
        }
    }
}
