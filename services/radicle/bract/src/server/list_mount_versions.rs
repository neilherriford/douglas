use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use super::{ClientErrorDisplay, Response};
use log::Logger;
use std::sync::Arc;

pub(super) struct ListMountVersions {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    token: Arc<TokenValidator>,
    version_manager: Arc<VersionManager>,
}
impl ListMountVersions {
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

    pub fn list(&self, token: String, service_name: String, mount_name: String) -> Response {
        self.log.info(&format!(
            "Listing mount versions available for {} {}",
            service_name, mount_name
        ));
        self.token.perform_if_valid(token, move || {
            let versions = or_log_and_return_error!(
                self.log => warn,
                self.version_manager.versions(&service_name, &mount_name)
            );
            Response::MountVersionsListed(versions)
        })
    }
}

#[cfg(test)]
mod tests {
    mod list_mount_versions {
        use super::super::ListMountVersions;
        use crate::{
            Version,
            server::{
                Response, mount_path_factory::MountPathFactory, token_validator::TokenValidator,
                version_manager::VersionManager,
            },
        };
        use credentials::MockCredentials;
        use file_system::{
            Entry, MockFileDeleter, MockFileReader, MockFolder, MockLinks, MockPermissions,
        };
        use log::MockLogger;
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

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
        ) -> ListMountVersions {
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

            ListMountVersions::new(
                logger.clone(),
                token_validator.clone(),
                version_manager.clone(),
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
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            log.expect_warn()
                .with(predicate::eq("Invalid token"))
                .return_const(());

            let actual = build(
                token_path,
                mount_root,
                Arc::new(log),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .list("foo".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_list() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            log.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            folder
                .given_exists("/tmp/mount_root/bar/baz/")
                .given_folder_entries(
                    "/tmp/mount_root/bar/baz/",
                    vec![Entry::create_directory("v0"), Entry::create_directory("v1")],
                );

            let actual = build(
                token_path,
                mount_root,
                Arc::new(log),
                Arc::new(file_reader),
                Arc::new(file_deleter),
                Arc::new(permissions),
                Arc::new(credentials),
                Arc::new(links),
                Arc::new(folder),
            )
            .list("token".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(
                actual,
                Response::MountVersionsListed(versions) if versions == vec![Version(0), Version(1)]
            ));
        }
    }
}
