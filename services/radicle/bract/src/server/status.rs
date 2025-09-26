use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use super::{ClientErrorDisplay, Response};
use log::Logger;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct Status {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    token: Arc<TokenValidator>,
    version_manager: Arc<VersionManager>,
    token_path: PathBuf,
    mount_root: PathBuf,
}

impl Status {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        token_validator: Arc<TokenValidator>,
        version_manager: Arc<VersionManager>,
        token_path: &Path,
        mount_root: &Path,
    ) -> Self {
        Self {
            log,
            token: token_validator,
            version_manager,
            token_path: token_path.to_path_buf(),
            mount_root: mount_root.to_path_buf(),
        }
    }

    pub fn perform(&self, token: String) -> Response {
        self.log.info("Reporting status");
        self.token.perform_if_valid(token, move || {
            let services = or_log_and_return_error!(
                self.log => warn,
                self.version_manager.list()
            );

            Response::Status {
                token_path: self.token_path.to_path_buf(),
                mount_root: self.mount_root.to_path_buf(),
                services,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod status {
        use super::super::*;
        use crate::Mount;
        use crate::{Service, Version, server::mount_path_factory::MountPathFactory};
        use credentials::MockCredentials;
        use file_system::{
            Entry, MockFileDeleter, MockFileReader, MockFolder, MockLinks, MockPermissions,
        };
        use log::MockLogger;
        use mockall::predicate;
        use std::path::Path;

        fn build(
            token_path: &Path,
            mount_root: &Path,
            logger: Arc<MockLogger>,
            file_reader: Arc<MockFileReader>,
            file_deleter: Arc<MockFileDeleter>,
            permissions: Arc<MockPermissions>,
            credentials: Arc<MockCredentials>,
            links: Arc<MockLinks>,
            folder: Arc<MockFolder>,
        ) -> Status {
            let mount_path_factory = Arc::new(MountPathFactory::new(
                mount_root,
                folder.clone(),
                links.clone(),
            ));

            let token_validator = Arc::new(TokenValidator::new(
                logger.clone(),
                file_reader.clone(),
                token_path,
            ));

            let version_manager = Arc::new(VersionManager::new(
                mount_path_factory.clone(),
                folder.clone(),
                links.clone(),
                file_deleter.clone(),
                permissions.clone(),
                credentials.clone(),
            ));

            Status::new(
                logger.clone(),
                token_validator.clone(),
                version_manager.clone(),
                &token_path,
                &mount_root,
            )
        }

        #[test]
        fn should_validate_token() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            log.expect_info().return_const(());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("token".to_string()));
            log.expect_warn()
                .with(predicate::eq("Invalid token"))
                .return_const(());

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(log),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .perform("foo".to_string());

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_return_status() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            log.expect_info().return_const(());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("token".to_string()));

            folder.given_folder_entries(
                "/tmp/mount_root/",
                vec![
                    Entry::create_directory("foo-service"),
                    Entry::create_directory("bar-service"),
                ],
            );

            folder
                .given_folder_entries(
                    "/tmp/mount_root/foo-service",
                    vec![
                        Entry::create_directory("bar-mount"),
                        Entry::create_directory("baz-mount"),
                    ],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount")
                .given_folder_entries(
                    "/tmp/mount_root/foo-service/bar-mount",
                    vec![
                        Entry::create_directory("current"),
                        Entry::create_directory("v0"),
                    ],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount/current")
                .given_exists("/tmp/mount_root/foo-service/bar-mount/v0")
                .given_exists("/tmp/mount_root/foo-service/baz-mount")
                .given_folder_entries(
                    "/tmp/mount_root/foo-service/baz-mount",
                    vec![
                        Entry::create_directory("current"),
                        Entry::create_directory("v0"),
                        Entry::create_directory("v1"),
                    ],
                )
                .given_exists("/tmp/mount_root/foo-service/baz-mount/current")
                .given_exists("/tmp/mount_root/foo-service/baz-mount/v0")
                .given_exists("/tmp/mount_root/foo-service/baz-mount/v1");

            folder
                .given_folder_entries(
                    "/tmp/mount_root/bar-service",
                    vec![Entry::create_directory("qux-mount")],
                )
                .given_exists("/tmp/mount_root/bar-service/qux-mount")
                .given_folder_entries(
                    "/tmp/mount_root/bar-service/qux-mount",
                    vec![
                        Entry::create_directory("current"),
                        Entry::create_directory("v0"),
                    ],
                )
                .given_exists("/tmp/mount_root/bar-service/qux-mount/current")
                .given_exists("/tmp/mount_root/bar-service/qux-mount/v0");

            links
                .given_symlink(
                    "/tmp/mount_root/foo-service/bar-mount/current",
                    "/tmp/mount_root/foo-service/bar-mount/v0",
                )
                .given_symlink(
                    "/tmp/mount_root/foo-service/baz-mount/current",
                    "/tmp/mount_root/foo-service/baz-mount/v1",
                )
                .given_symlink(
                    "/tmp/mount_root/bar-service/qux-mount/current",
                    "/tmp/mount_root/bar-service/qux-mount/v0",
                );

            folder
                .expect_pop_with("/tmp/mount_root/foo-service/bar-mount/v0", "v0")
                .expect_pop_with("/tmp/mount_root/foo-service/baz-mount/v0", "v0")
                .expect_pop_with("/tmp/mount_root/foo-service/baz-mount/v1", "v1")
                .expect_pop_with("/tmp/mount_root/bar-service/qux-mount/v0", "v0");

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(log),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .perform("token".to_string());

            let expected = Response::Status {
                token_path: token_path.to_path_buf(),
                mount_root: mount_root.to_path_buf(),
                services: vec![
                    Service {
                        name: "foo-service".to_string(),
                        mounts: vec![
                            Mount {
                                path: Path::new("/tmp/mount_root/foo-service/bar-mount/current")
                                    .to_path_buf(),
                                version: Version(0),
                                name: "bar-mount".to_string(),
                            },
                            Mount {
                                path: Path::new("/tmp/mount_root/foo-service/baz-mount/current")
                                    .to_path_buf(),
                                version: Version(1),
                                name: "baz-mount".to_string(),
                            },
                        ],
                    },
                    Service {
                        name: "bar-service".to_string(),
                        mounts: vec![Mount {
                            path: Path::new("/tmp/mount_root/bar-service/qux-mount/current")
                                .to_path_buf(),
                            version: Version(0),
                            name: "qux-mount".to_string(),
                        }],
                    },
                ],
            };

            assert_eq!(expected, actual);
        }
    }
}
