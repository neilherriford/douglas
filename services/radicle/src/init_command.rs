use crate::{
    config::{Config, ConfigRepositoryError, ConfigWriter},
    constants,
};
use credentials::{Credentials, CredentialsError};
use file_system::{FileSystemError, Folder, Modes, Permissions};
use log::Logger;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum InitCommandError {
    #[error("Must be root to intialize the system")]
    NotRootError,
    #[error("The provided service user, '{0}' does not exist")]
    ServiceUserNotFound(String),
    #[error("Credentials error '{0}'")]
    CredentialsError(#[from] CredentialsError),
    #[error("File system error '{0}'")]
    FileSystemError(#[from] FileSystemError),
    #[error("Configuration file error '{0}'")]
    ConfigRepositoryError(#[from] ConfigRepositoryError),
}

pub struct InitCommand {
    service_user: String,
    service_group: String,
    mount_root_path: PathBuf,
    log_path: PathBuf,
    docker_socket_path: PathBuf,
    log: Arc<dyn Logger>,
    credentials: Arc<dyn Credentials>,
    folder: Arc<dyn Folder>,
    permissions: Arc<dyn Permissions>,
    config_writer: Arc<dyn ConfigWriter>,
}

impl InitCommand {
    pub fn new(
        log: Arc<dyn Logger>,
        service_user: &str,
        service_group: &str,
        mount_root_path: &Path,
        log_path: &Path,
        docker_socket_path: &Path,
        credentials: Arc<dyn Credentials>,
        folder: Arc<dyn Folder>,
        permissions: Arc<dyn Permissions>,
        config_writer: Arc<dyn ConfigWriter>,
    ) -> Self {
        Self {
            log: Arc::clone(&log),
            service_user: service_user.to_string(),
            service_group: service_group.to_string(),
            mount_root_path: mount_root_path.to_path_buf(),
            log_path: log_path.to_path_buf(),
            docker_socket_path: docker_socket_path.to_path_buf(),
            credentials: Arc::clone(&credentials),
            folder: Arc::clone(&folder),
            permissions: Arc::clone(&permissions),
            config_writer: Arc::clone(&config_writer),
        }
    }

    pub fn run(&self) -> Result<(), InitCommandError> {
        self.assert_root()?;
        self.create_system_credentials()?;
        self.assert_service_user_exists()?;
        self.add_service_user()?;
        self.log.info("Creating mount path");
        self.create_system_path(&self.mount_root_path.as_path())?;
        self.set_ownership_and_permissions(&self.mount_root_path.as_path())?;
        self.log.info("Creating log path");
        self.create_system_path(&self.log_path.as_path())?;
        self.write_config()
    }

    fn assert_root(&self) -> Result<(), InitCommandError> {
        if self.credentials.is_root() {
            Ok(())
        } else {
            Err(InitCommandError::NotRootError)
        }
    }

    fn create_system_credentials(&self) -> Result<(), CredentialsError> {
        self.create_group(constants::DOUGLAS_GROUP)?;
        self.create_group(constants::RADICLE_GROUP)?;

        self.log
            .info(&format!("creating '{}' user", constants::RADICLE_USER));
        self.credentials.create_user(
            constants::RADICLE_USER,
            constants::RADICLE_GROUP,
            vec![constants::DOUGLAS_GROUP.to_string()],
        )?;

        Ok(())
    }

    fn assert_service_user_exists(&self) -> Result<(), InitCommandError> {
        if self.credentials.user_exists(&self.service_user) {
            Ok(())
        } else {
            Err(InitCommandError::ServiceUserNotFound(
                self.service_user.to_string(),
            ))
        }
    }

    fn add_service_user(&self) -> Result<(), CredentialsError> {
        self.log.info(&format!(
            "Making user '{}' an operator by adding to '{}' group",
            self.service_user,
            constants::RADICLE_GROUP
        ));
        self.credentials
            .join_group(&self.service_user, constants::RADICLE_GROUP)
    }

    fn create_system_path(&self, path: &Path) -> Result<(), InitCommandError> {
        self.folder.create_recursively(&path)?;
        Ok(())
    }

    fn set_ownership_and_permissions(&self, path: &Path) -> Result<(), FileSystemError> {
        self.log.info("Setting mount path permissions");
        self.permissions.change_user_and_group_ownership(
            path,
            &self.service_user,
            &self.service_group,
        )?;
        self.permissions
            .change_mode(path, &Modes::OwnerReadWriteGroupReadWrite)
    }

    fn write_config(&self) -> Result<(), InitCommandError> {
        self.log.info("Writing config");

        let config = Config {
            operator_user: self.service_user.to_string(),
            operator_group: self.service_group.to_string(),
            mount_root_path: self.mount_root_path.clone(),
            log_path: self.log_path.clone(),
            docker_socket_path: self.docker_socket_path.clone(),
        };

        self.config_writer.save(&config)?;
        Ok(())
    }

    fn create_group(&self, name: &str) -> Result<(), CredentialsError> {
        self.log.info(&format!("Creating '{}' group", name));
        self.credentials.create_group(name)
    }
}

#[cfg(test)]
mod tests {
    use super::InitCommand;
    use crate::config::MockConfigWriter;
    use credentials::MockCredentials;
    use file_system::{MockFolder, MockPermissions};
    use log::MockLogger;
    use std::{path::Path, sync::Arc};

    fn build(
        log: Arc<MockLogger>,
        service_user: &str,
        service_group: &str,
        mount_root_path: &str,
        log_path: &str,
        docker_socket_path: &str,
        credentials: Arc<MockCredentials>,
        folder: Arc<MockFolder>,
        permissions: Arc<MockPermissions>,
        config_writer: Arc<MockConfigWriter>,
    ) -> InitCommand {
        InitCommand::new(
            log.clone(),
            service_user,
            service_group,
            Path::new(mount_root_path),
            Path::new(log_path),
            Path::new(docker_socket_path),
            credentials.clone(),
            folder.clone(),
            permissions.clone(),
            config_writer.clone(),
        )
    }
    mod run {
        use std::path::PathBuf;
        use std::{path::Path, sync::Arc};

        use super::build;
        use crate::config::{Config, ConfigRepositoryError, MockConfigWriter};
        use crate::init_command::InitCommandError;
        use credentials::{CredentialsError, MockCredentials};
        use file_system::{FileSystemError, MockFolder, MockPermissions, Modes};
        use log::MockLogger;
        use mockall::predicate;

        #[test]
        fn should_err_if_not_root() {
            let log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            credentials.given_is_not_root();

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::NotRootError)));
        }

        #[test]
        fn should_err_if_douglas_group_creation_fails() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .returning(|_| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::CredentialsError(_))));
        }

        #[test]
        fn should_err_if_radicle_group_creation_fails() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials
                .given_is_root()
                .expect_group_created_named("douglas");
            credentials
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .returning(|_| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::CredentialsError(_))));
        }

        #[test]
        fn should_err_if_radicle_user_fails() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials
                .given_is_root()
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle");

            credentials
                .expect_create_user()
                .with(
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::CredentialsError(_))));
        }

        #[test]
        fn should_err_if_service_user_does_not_exist() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials
                .given_is_root()
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_does_not_exist("foo_user");

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(
                actual,
                Err(InitCommandError::ServiceUserNotFound(_))
            ));
        }

        #[test]
        fn should_err_if_service_user_cannot_join_group() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user");

            credentials
                .expect_join_group()
                .with(predicate::eq("foo_user"), predicate::eq("doug-radicle"))
                .returning(|_, _| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::CredentialsError(_))));
        }

        #[test]
        fn should_err_if_mount_root_could_not_be_created() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder
                .expect_create_recursively()
                .with(predicate::eq(Path::new("/tmp/mounts")))
                .returning(|_| Err(file_system::FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::FileSystemError(_))));
        }

        #[test]
        fn should_err_if_mount_permissions_could_not_be_set() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder.expect_create_folder_recursively_with("/tmp/mounts");
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mounts")),
                    predicate::eq("foo_user"),
                    predicate::eq("foo_group"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::FileSystemError(_))));
        }

        #[test]
        fn should_err_if_mount_ownership_could_not_be_set() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder.expect_create_folder_recursively_with("/tmp/mounts");
            permissions.expect_ownership_to_be_set("/tmp/mounts", "foo_user", "foo_group");

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mounts")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::FileSystemError(_))));
        }

        #[test]
        fn should_err_if_log_directory_could_not_be_created() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder.expect_create_folder_recursively_with("/tmp/mounts");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mounts",
                "foo_user",
                "foo_group",
                Modes::OwnerReadWriteGroupReadWrite,
            );
            folder
                .expect_create_recursively()
                .with(predicate::eq(Path::new("/tmp/logs")))
                .returning(|_| Err(file_system::FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Err(InitCommandError::FileSystemError(_))));
        }

        #[test]
        fn should_err_if_config_cannot_be_saved() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let mut config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder.expect_create_folder_recursively_with("/tmp/mounts");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mounts",
                "foo_user",
                "foo_group",
                Modes::OwnerReadWriteGroupReadWrite,
            );
            folder.expect_create_folder_recursively_with("/tmp/logs");

            let config = Config {
                log_path: PathBuf::from("/tmp/logs"),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                operator_group: "foo_group".to_string(),
                operator_user: "foo_user".to_string(),
                docker_socket_path: Path::new("/tmp/docker.socket").to_path_buf(),
            };

            config_writer
                .expect_save()
                .with(predicate::eq(config))
                .returning(|_| {
                    Err(ConfigRepositoryError::FileSystemError(
                        FileSystemError::ExpectedFileError,
                    ))
                });

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(
                actual,
                Err(InitCommandError::ConfigRepositoryError(_))
            ));
        }
        #[test]
        fn should_run() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let mut config_writer = MockConfigWriter::new();

            log.expect_info().return_const(());
            credentials.given_is_root();
            credentials
                .expect_group_created_named("douglas")
                .expect_group_created_named("doug-radicle")
                .expect_user_created_named("doug-radicle", "doug-radicle", vec!["douglas"])
                .given_user_exists("foo_user")
                .expect_join_group_with("foo_user", "doug-radicle");

            folder.expect_create_folder_recursively_with("/tmp/mounts");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mounts",
                "foo_user",
                "foo_group",
                Modes::OwnerReadWriteGroupReadWrite,
            );
            folder.expect_create_folder_recursively_with("/tmp/logs");
            config_writer.expect_save_with(Config {
                log_path: PathBuf::from("/tmp/logs"),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                operator_group: "foo_group".to_string(),
                operator_user: "foo_user".to_string(),
                docker_socket_path: Path::new("/tmp/docker.socket").to_path_buf(),
            });

            let actual = build(
                Arc::new(log),
                "foo_user",
                "foo_group",
                "/tmp/mounts",
                "/tmp/logs",
                "/tmp/docker.socket",
                Arc::new(credentials),
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(config_writer),
            )
            .run();

            assert!(matches!(actual, Ok(())));
        }
    }
}
