use super::ServerError;
use file_system::{FileDeleter, Listener, Modes, Permissions, UnixDomainSocket};
use log::Logger;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct CreateListener {
    log: Arc<dyn Logger + Sync + Send>,
    socket_path: PathBuf,
    file_deleter: Arc<dyn FileDeleter>,
    permissions: Arc<dyn Permissions>,
    unix_domain_socket: Arc<dyn UnixDomainSocket>,
}

impl CreateListener {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        socket_path: &Path,
        file_deleter: Arc<dyn FileDeleter>,
        permissions: Arc<dyn Permissions>,
        unix_domain_socket: Arc<dyn UnixDomainSocket>,
    ) -> Self {
        Self {
            log,
            socket_path: socket_path.to_path_buf(),
            file_deleter,
            permissions,
            unix_domain_socket,
        }
    }

    pub fn create(&self) -> Result<Box<dyn Listener + Send + Sync + 'static>, ServerError> {
        self.log.info(&format!(
            "Refreshing socket '{}'",
            self.socket_path.to_string_lossy()
        ));
        self.file_deleter.delete(&self.socket_path)?;

        let listener = self.unix_domain_socket.bind(&self.socket_path)?;

        self.permissions.change_user_and_group_ownership(
            &self.socket_path,
            credentials::ROOT_USER_NAME,
            config::constants::RADICLE_GROUP,
        )?;
        self.permissions
            .change_mode(&self.socket_path, &Modes::OwnerReadWriteGroupReadWrite)?;

        self.log.info("  done!");
        Ok(listener)
    }
}

#[cfg(test)]
mod tests {
    mod create_listener {
        use super::super::CreateListener;
        use file_system::{
            FileSystemError, MockFileDeleter, MockListener, MockPermissions, MockUnixDomainSocket,
            Modes,
        };
        use log::MockLogger;
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_return_error_if_socket_could_not_be_deleted() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().return_const(());
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .returning(|_| {
                    Err(FileSystemError::NotFoundError(
                        Path::new("/tmp/socket").to_path_buf(),
                    ))
                });

            let actual = CreateListener::new(
                Arc::new(log),
                socket_path,
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
            )
            .create();

            assert!(actual.is_err());
        }

        #[test]
        fn should_return_error_if_bind_failed() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().return_const(());
            file_deleter.expect_file_to_be_deleted("/tmp/socket");
            unix_domain_socket
                .expect_bind()
                .with(predicate::eq(Path::new("/tmp/socket")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                socket_path,
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
            )
            .create();

            assert!(actual.is_err());
        }

        #[test]
        fn should_return_error_if_permissions_ownership_failed() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().return_const(());
            file_deleter.expect_file_to_be_deleted("/tmp/socket");
            unix_domain_socket.expect_bind_with("/tmp/socket", || Box::new(MockListener::new()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq("root"),
                    predicate::eq("doug-radicle"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                socket_path,
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
            )
            .create();

            assert!(actual.is_err());
        }

        #[test]
        fn should_return_error_if_permissions_mode_failed() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().return_const(());
            file_deleter.expect_file_to_be_deleted("/tmp/socket");
            unix_domain_socket.expect_bind_with("/tmp/socket", || Box::new(MockListener::new()));
            permissions.expect_ownership_to_be_set("/tmp/socket", "root", "doug-radicle");
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/socket")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = CreateListener::new(
                Arc::new(log),
                socket_path,
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
            )
            .create();

            assert!(actual.is_err());
        }

        #[test]
        fn should_create_listener() {
            let mut log = MockLogger::new();
            let socket_path = Path::new("/tmp/socket");
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut unix_domain_socket = MockUnixDomainSocket::new();

            log.expect_info().return_const(());
            file_deleter.expect_file_to_be_deleted("/tmp/socket");
            unix_domain_socket.expect_bind_with("/tmp/socket", || Box::new(MockListener::new()));
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/socket",
                "root",
                "doug-radicle",
                Modes::OwnerReadWriteGroupReadWrite,
            );

            let actual = CreateListener::new(
                Arc::new(log),
                socket_path,
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(unix_domain_socket),
            )
            .create();

            assert!(actual.is_ok());
        }
    }
}
