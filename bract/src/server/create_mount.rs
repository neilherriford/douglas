use super::Response;
use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use crate::version::Version;
use log::Logger;
use std::sync::Arc;
use utils::ClientErrorDisplay;

pub(super) struct CreateMount {
    token: Arc<TokenValidator>,
    log: Arc<dyn Logger + Sync + Send>,
    version_manager: Arc<VersionManager>,
}

impl CreateMount {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        token_validator: Arc<TokenValidator>,
        version_manager: Arc<VersionManager>,
    ) -> Self {
        Self {
            log,
            token: token_validator,
            version_manager,
        }
    }

    pub fn create(
        &self,
        token: String,
        service_name: String,
        mount_name: String,
        shared: bool,
    ) -> Response {
        self.log.info(&format!(
            "Creating mount for {} {}",
            service_name, mount_name
        ));
        self.token.perform_if_valid(token, move || {
            let version = Version(0);
            let active_path = or_log_and_return_response_error!(
                self.log => warn,
                self.version_manager.create(
                    &service_name,
                    &mount_name,
                    version,
                    shared
                )
            );

            Response::MountSet {
                name: mount_name,
                version,
                path: active_path.to_path_buf(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod create_mount {
        use super::super::CreateMount;
        use crate::Version;
        use crate::server::Response;
        use crate::server::create_mount::{TokenValidator, VersionManager};
        use crate::server::mount_path_factory::MountPathFactory;
        use credentials::MockCredentials;
        use file_system::{
            MockFileDeleter, MockFileReader, MockFolder, MockLinks, MockPermissions, Modes,
        };
        use log::MockLogger;
        use mockall::predicate;
        use std::path::Path;
        use std::sync::Arc;

        #[allow(clippy::too_many_arguments)]
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
        ) -> CreateMount {
            let mount_path_factory = Arc::new(MountPathFactory::new(
                mount_root,
                folder.clone(),
                links.clone(),
            ));

            let version_manager = Arc::new(VersionManager::new(
                mount_path_factory.clone(),
                folder.clone(),
                links.clone(),
                file_deleter.clone(),
                permissions.clone(),
                credentials.clone(),
            ));

            let token_validator = Arc::new(TokenValidator::new(
                logger.clone(),
                file_reader.clone(),
                token_path,
            ));

            CreateMount::new(
                logger.clone(),
                token_validator.clone(),
                version_manager.clone(),
            )
        }

        #[test]
        fn should_verify_the_token() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let links = MockLinks::new();
            let folder = MockFolder::new();

            logger.expect_info().return_const(());
            logger
                .expect_warn()
                .with(predicate::eq("Invalid token"))
                .return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");

            let instance = build(
                Path::new("/tmp/token"),
                Path::new("/tmp/mount_root"),
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            );

            let actual = instance.create(
                "foo".to_string(),
                "bar".to_string(),
                "baz".to_string(),
                false,
            );

            assert_eq!(Response::InvalidToken, actual);
        }

        #[test]
        fn should_create_mount() {
            let mut logger = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mut links: MockLinks = MockLinks::new();
            let mut folder = MockFolder::new();

            logger.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            credentials.given_user_and_group_exist("doug-bar", "doug-bar");
            folder
                .given_exists("/tmp/mount_root/")
                .given_exists("/tmp/mount_root/bar")
                .given_does_not_exist("/tmp/mount_root/bar/baz")
                .given_does_not_exist("/tmp/mount_root/bar/baz/current")
                .given_does_not_exist("/tmp/mount_root/bar/baz/v0")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz")
                .expect_create_folder_recursively_with("/tmp/mount_root/bar/baz/v0");

            permissions
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar",
                    "doug-bar",
                    "douglas",
                    Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz",
                    "doug-bar",
                    "douglas",
                    Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/v0",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
                )
                .expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/bar/baz/current",
                    "doug-bar",
                    "doug-bar",
                    Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
                );

            folder.given_folder_entries("/tmp/mount_root/bar/baz", vec![]);

            links.expect_create_with(
                "/tmp/mount_root/bar/baz/v0",
                "/tmp/mount_root/bar/baz/current",
            );

            let actual = build(
                Path::new("/tmp/token"),
                Path::new("/tmp/mount_root"),
                Arc::new(logger),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .create(
                "token".to_string(),
                "bar".to_string(),
                "baz".to_string(),
                false,
            );

            assert!(matches!(actual, Response::MountSet {
                    name,
                    version,
                    path
                } if name == "baz" && version == Version(0) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }
    }
}
