use file_system::{FileReader, FileSystemError, FileWriter, Folder, Modes, Permissions};
use mockall::automock;
#[cfg(test)]
use mockall::predicate;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Config {
    pub operator_user: String,
    pub operator_group: String,
    pub mount_root_path: PathBuf,
    pub log_path: PathBuf,
    pub docker_socket_path: PathBuf,
}

#[derive(Error, Debug)]
pub enum ConfigRepositoryError {
    #[error("Serialization error '{0}'")]
    SerializationError(#[from] serde_json::Error),
    #[error("File system error '{0}'")]
    FileSystemError(#[from] FileSystemError),
}

#[automock]
pub trait ConfigReader {
    fn read(&self) -> Result<Config, ConfigRepositoryError>;
}

#[automock]
pub trait ConfigWriter {
    fn save(&self, config: &Config) -> Result<(), ConfigRepositoryError>;
}

pub struct LocalConfigRepository {
    folder: Arc<dyn Folder + Send + Sync + 'static>,
    permissions: Arc<dyn Permissions + Send + Sync + 'static>,
    file_writer: Arc<dyn FileWriter + Send + Sync + 'static>,
    file_reader: Arc<dyn FileReader + Send + Sync + 'static>,
}

impl LocalConfigRepository {
    pub fn new(
        folder: Arc<dyn Folder + Send + Sync + 'static>,
        permissions: Arc<dyn Permissions + Send + Sync + 'static>,
        file_reader: Arc<dyn FileReader + Send + Sync + 'static>,
        file_writer: Arc<dyn FileWriter + Send + Sync + 'static>,
    ) -> Self {
        Self {
            folder,
            permissions,
            file_writer,
            file_reader,
        }
    }

    fn config_path(&self) -> Result<PathBuf, FileSystemError> {
        let mut path = self.folder.executable_root()?;
        path.push("douglas-config.json");
        Ok(path)
    }
}

impl ConfigReader for LocalConfigRepository {
    fn read(&self) -> Result<Config, ConfigRepositoryError> {
        let path = self.config_path()?;
        let data = self.file_reader.read_all(path.as_path())?;
        let config: Config = serde_json::from_str(&data)?;

        Ok(config)
    }
}

impl ConfigWriter for LocalConfigRepository {
    fn save(&self, config: &Config) -> Result<(), ConfigRepositoryError> {
        let path = self.config_path()?;
        let path = path.as_path();
        let json = serde_json::to_string_pretty(config)?;

        self.file_writer.write_all(path, json)?;
        self.permissions.change_user_and_group_ownership(
            path,
            &config.operator_user,
            &config.operator_group,
        )?;
        self.permissions
            .change_mode(path, &Modes::OwnerReadWriteGroupReadWrite)?;

        Ok(())
    }
}

#[cfg(test)]
impl MockConfigWriter {
    pub fn expect_save_with(&mut self, config: Config) -> &mut Self {
        self.expect_save()
            .with(predicate::eq(config))
            .returning(|_| Ok(()));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::LocalConfigRepository;
    use file_system::{MockFileReader, MockFileWriter, MockFolder, MockPermissions};
    use std::sync::Arc;

    fn build(
        folder: Arc<MockFolder>,
        permissions: Arc<MockPermissions>,
        file_reader: Arc<MockFileReader>,
        file_writer: Arc<MockFileWriter>,
    ) -> LocalConfigRepository {
        LocalConfigRepository::new(
            folder.clone(),
            permissions.clone(),
            file_reader.clone(),
            file_writer.clone(),
        )
    }

    mod save {
        use super::build;
        use crate::config::{ConfigRepositoryError, ConfigWriter};
        use file_system::{
            FileSystemError, MockFileReader, MockFileWriter, MockFolder, MockPermissions, Modes,
        };
        use mockall::predicate;
        use std::{
            path::{Path, PathBuf},
            sync::Arc,
        };

        #[test]
        fn should_error_if_config_path_could_not_be_determined() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();

            folder
                .expect_executable_root()
                .returning(|| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .save(&crate::config::Config {
                operator_user: "foo-operator".to_string(),
                operator_group: "foo-group".to_string(),
                mount_root_path: PathBuf::from("/tmp/mount"),
                log_path: Path::new("/tmp/log").to_path_buf(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_err_if_write_fails() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_writer
                .expect_write_all()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::always(),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .save(&crate::config::Config {
                operator_user: "foo-operator".to_string(),
                operator_group: "foo-group".to_string(),
                mount_root_path: PathBuf::from("/tmp/mount"),
                log_path: Path::new("/tmp/log").to_path_buf(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_err_if_permissions_fail() {
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_writer
                .expect_write_all()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::always(),
                )
                .returning(|_, _| Ok(()));

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::eq("foo-operator".to_string()),
                    predicate::eq("foo-group".to_string()),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .save(&crate::config::Config {
                operator_user: "foo-operator".to_string(),
                operator_group: "foo-group".to_string(),
                mount_root_path: PathBuf::from("/tmp/mount"),
                log_path: Path::new("/tmp/log").to_path_buf(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_err_if_mode_fails() {
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_writer
                .expect_write_all()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::always(),
                )
                .returning(|_, _| Ok(()));

            permissions.expect_ownership_to_be_set(
                "/tmp/douglas-config.json",
                "foo-operator",
                "foo-group",
            );
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .save(&crate::config::Config {
                operator_user: "foo-operator".to_string(),
                operator_group: "foo-group".to_string(),
                mount_root_path: PathBuf::from("/tmp/mount"),
                log_path: Path::new("/tmp/log").to_path_buf(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_save() {
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_writer
                .expect_write_all()
                .with(
                    predicate::eq(Path::new("/tmp/douglas-config.json")),
                    predicate::always(),
                )
                .returning(|_, _| Ok(()));

            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/douglas-config.json",
                "foo-operator",
                "foo-group",
                Modes::OwnerReadWriteGroupReadWrite,
            );

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .save(&crate::config::Config {
                operator_user: "foo-operator".to_string(),
                operator_group: "foo-group".to_string(),
                mount_root_path: PathBuf::from("/tmp/mount"),
                log_path: Path::new("/tmp/log").to_path_buf(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            });

            assert!(matches!(actual, Ok(())));
        }
    }

    mod load {
        use super::build;
        use crate::config::{Config, ConfigReader, ConfigRepositoryError};
        use file_system::{
            FileSystemError, MockFileReader, MockFileWriter, MockFolder, MockPermissions,
        };
        use mockall::predicate;
        use std::{
            path::{Path, PathBuf},
            sync::Arc,
        };

        #[test]
        fn should_error_if_config_path_could_not_be_determined() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();

            folder
                .expect_executable_root()
                .returning(|| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .read();

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_file_could_not_be_read() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/douglas-config.json")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .read();

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_the_config_is_corrupt() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/douglas-config.json", "oops");

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .read();

            assert!(matches!(
                actual,
                Err(ConfigRepositoryError::SerializationError(_))
            ));
        }

        #[test]
        fn should_read() {
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();

            let config = r#" {
              "operator_user": "foo",
              "operator_group": "bar",
              "mount_root_path": "/tmp/mounts",
              "log_path": "/tmp/logs",
              "docker_socket_path": "/tmp/docker.socket"
            }
            "#;

            folder.given_executable_root("/tmp");
            file_reader.given_can_read_all_with_contents("/tmp/douglas-config.json", config);

            let actual = build(
                Arc::new(folder),
                Arc::new(permissions),
                Arc::new(file_reader),
                Arc::new(file_writer),
            )
            .read();

            let expected = Config {
                log_path: PathBuf::from("/tmp/logs"),
                mount_root_path: PathBuf::from("/tmp/mounts"),
                operator_group: "bar".to_string(),
                operator_user: "foo".to_string(),
                docker_socket_path: PathBuf::from("/tmp/docker.socket"),
            };

            assert!(matches!(actual, Ok(config) if config == expected));
        }
    }
}
