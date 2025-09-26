use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use super::{ClientErrorDisplay, Response};
use crate::version::Version;
use log::Logger;
use std::sync::Arc;

pub(super) struct CreateNewMountVersion {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    token: Arc<TokenValidator>,
    version_manager: Arc<VersionManager>,
}

impl CreateNewMountVersion {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        token_validator: Arc<TokenValidator>,
        version_manager: Arc<VersionManager>,
    ) -> Self {
        Self {
            log,
            token: token_validator,
            version_manager,
        }
    }

    pub fn create(&self, token: String, service_name: String, mount_name: String) -> Response {
        self.log.info(&format!(
            "Creating new mount version for {} {}",
            service_name, mount_name
        ));
        self.token.perform_if_valid(token, move || {
            let new_version = if self
                .version_manager
                .is_initialized(&service_name, &mount_name)
            {
                match or_log_and_return_error!(
                    self.log => warn,
                    self.version_manager.versions(&service_name, &mount_name)
                )
                .as_slice()
                {
                    [] => Version(0),
                    [.., last] => last.to_owned() + 1,
                }
            } else {
                Version(0)
            };

            let path = or_log_and_return_error!(self.log => warn,
                self.version_manager.create(
                    &service_name,
                    &mount_name,
                    new_version
                )
            );

            Response::MountSet {
                name: mount_name,
                version: new_version,
                path,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod create_new_mount_version {
        use super::super::CreateNewMountVersion;
        use crate::{
            Version,
            server::{
                Response, mount_path_factory::MountPathFactory, token_validator::TokenValidator,
                version_manager::VersionManager,
            },
        };
        use credentials::MockCredentials;
        use file_system::{
            Entry, MockFileDeleter, MockFileReader, MockFolder, MockLinks, MockPermissions, Modes,
        };
        use log::MockLogger;
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

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
        ) -> CreateNewMountVersion {
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

            CreateNewMountVersion::new(
                logger.clone(),
                token_validator.clone(),
                version_manager.clone(),
            )
        }

        #[test]
        fn should_validate_token() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();

            let links: MockLinks = MockLinks::new();
            let folder = MockFolder::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            logger.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            logger
                .expect_warn()
                .with(predicate::eq("Invalid token"))
                .return_const(());

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .create("foo".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_create_if_not_intialized() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mut links: MockLinks = MockLinks::new();
            let mut folder = MockFolder::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            logger.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            credentials.given_user_and_group_exist("doug-bar", "doug-bar");
            folder
                .given_does_not_exist("/tmp/mount_root/")
                .given_does_not_exist("/tmp/mount_root/bar")
                .given_does_not_exist("/tmp/mount_root/bar/baz")
                .given_does_not_exist("/tmp/mount_root/bar/baz/current")
                .given_does_not_exist("/tmp/mount_root/bar/baz/v0")
                .expect_create_folder_recursively_with("/tmp/mount_root")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz/v0");

            permissions
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root",
                    "root",
                    "root",
                    Modes::OwnerReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/v0",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/current",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                );

            links.expect_create_with(
                "/tmp/mount_root/bar/baz/v0",
                "/tmp/mount_root/bar/baz/current",
            );

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .create("token".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(actual, Response::MountSet {
                    name,
                    version,
                    path
                } if name == "baz" && version == Version(0) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }

        #[test]
        fn should_create_if_intialized() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mut links: MockLinks = MockLinks::new();
            let mut folder = MockFolder::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            logger.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            credentials.given_user_and_group_exist("doug-bar", "doug-bar");
            folder
                .given_exists("/tmp/mount_root/")
                .given_does_not_exist("/tmp/mount_root/bar")
                .given_does_not_exist("/tmp/mount_root/bar/baz")
                .given_does_not_exist("/tmp/mount_root/bar/baz/current")
                .given_does_not_exist("/tmp/mount_root/bar/baz/v0")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz/v0");

            permissions
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/v0",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/current",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                );

            links.expect_create_with(
                "/tmp/mount_root/bar/baz/v0",
                "/tmp/mount_root/bar/baz/current",
            );

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .create("token".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(actual, Response::MountSet {
                    name,
                    version,
                    path
                } if name == "baz" && version == Version(0) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }

        #[test]
        fn should_create_if_mount_exist_with_versions() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mut links: MockLinks = MockLinks::new();
            let mut folder = MockFolder::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            logger.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            credentials.given_user_and_group_exist("doug-bar", "doug-bar");
            folder
                .given_exists("/tmp/mount_root/")
                .given_exists("/tmp/mount_root/bar")
                .given_exists("/tmp/mount_root/bar/baz")
                .given_exists("/tmp/mount_root/bar/baz/current")
                .given_exists("/tmp/mount_root/bar/baz/v0")
                .given_does_not_exist("/tmp/mount_root/bar/baz/v1")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz/v1");

            permissions
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/v1",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/current",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteGroupReadWrite,
                );

            folder.given_folder_entries(
                "/tmp/mount_root/bar/baz",
                vec![Entry::create_directory("v0")],
            );

            file_deleter.expect_file_to_be_deleted("/tmp/mount_root/bar/baz/current");
            links.expect_create_with(
                "/tmp/mount_root/bar/baz/v1",
                "/tmp/mount_root/bar/baz/current",
            );

            let actual = build(
                &token_path,
                &mount_root,
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .create("token".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(actual, Response::MountSet {
                name,
                    version,
                    path
                } if name == "baz" && version == Version(1) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }
    }
}
