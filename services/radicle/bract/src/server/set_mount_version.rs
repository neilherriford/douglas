use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use super::{ClientErrorDisplay, Response};
use crate::version::Version;
use log::Logger;
use std::sync::Arc;

pub(super) struct SetMountVersion {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    token: Arc<TokenValidator>,
    version_manager: Arc<VersionManager>,
}

impl SetMountVersion {
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

    pub fn perform(
        &self,
        token: String,
        service_name: String,
        mount_name: String,
        version: Version,
    ) -> Response {
        self.log.info(&format!(
            "Requesting active version for {} {} to {}",
            service_name, mount_name, version
        ));

        self.token.perform_if_valid(token, move || {
            let path = or_log_and_return_error!(
                self.log => warn,
                self.version_manager.activate(
                    &service_name,
                    &mount_name,
                    version
                )
            );

            Response::MountSet {
                name: mount_name,
                version,
                path,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod set_mount_version {
        use super::super::*;
        use crate::server::mount_path_factory::MountPathFactory;
        use credentials::MockCredentials;
        use file_system::{
            MockFileDeleter, MockFileReader, MockFolder, MockLinks, MockPermissions, Modes,
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
        ) -> SetMountVersion {
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

            SetMountVersion::new(
                logger.clone(),
                token_validator.clone(),
                version_manager.clone(),
            )
        }

        #[test]
        fn should_validate_token() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");

            let folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();

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
            .perform(
                "foo".to_string(),
                "bar".to_string(),
                "baz".to_string(),
                Version(0),
            );

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_perform() {
            let mut log = MockLogger::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");
            let mount_root = Path::new("/tmp/mount_root/");
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();

            log.expect_info().return_const(());
            file_reader
                .expect_read_all()
                .with(predicate::eq(Path::new("/tmp/token")))
                .returning(|_| Ok("token".to_string()));

            credentials.given_user_and_group_exist("doug-bar", "doug-bar");
            folder.given_exists("/tmp/mount_root/bar/baz/current");
            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/mount_root/bar/baz/current")))
                .returning(|_| Ok(()));
            links
                .expect_create()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/bar/baz/v0")),
                    predicate::eq(Path::new("/tmp/mount_root/bar/baz/current")),
                )
                .returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/bar/baz/current")),
                    predicate::eq("doug-bar"),
                    predicate::eq("doug-bar"),
                )
                .returning(|_, _, _| Ok(()));

            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/bar/baz/current")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Ok(()));

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
            .perform(
                "token".to_string(),
                "bar".to_string(),
                "baz".to_string(),
                Version(0),
            );

            assert!(matches!(
                actual,
                Response::MountSet {
                    name,
                    version,
                    path
                } if name == "baz" && version == Version(0) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }
    }
}
