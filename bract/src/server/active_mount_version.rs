use super::mount_path_factory::MountPathFactory;
use super::token_validator::TokenValidator;
use super::{ClientErrorDisplay, Response};
use log::Logger;
use std::sync::Arc;

pub(super) struct ActiveMountVersion {
    token: Arc<TokenValidator>,
    mount_paths: Arc<MountPathFactory>,
    log: Arc<dyn Logger>,
}
impl ActiveMountVersion {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        token_validator: Arc<TokenValidator>,
        mount_path_factory: Arc<MountPathFactory>,
    ) -> Self {
        Self {
            log,
            token: token_validator,
            mount_paths: mount_path_factory,
        }
    }

    pub fn perform(&self, token: String, service_name: String, mount_name: String) -> Response {
        self.log.info(&format!(
            "Requesting active version for {} {}",
            service_name, mount_name
        ));
        self.token.perform_if_valid(token, move || {
            let version = or_log_and_return_error!(
                self.log => warn,
                self.mount_paths.active_version(&service_name, &mount_name)
            );
            Response::MountSet {
                name: mount_name.to_string(),
                version,
                path: self
                    .mount_paths
                    .active_version_path(&service_name, &mount_name)
                    .to_path_buf(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod active_mount_version {
        use super::super::ActiveMountVersion;
        use crate::{
            Version,
            server::{
                Response, mount_path_factory::MountPathFactory, token_validator::TokenValidator,
            },
        };
        use file_system::{MockFileReader, MockFolder, MockLinks};
        use log::MockLogger;
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

        fn build(
            token_path: &Path,
            mount_root: &Path,
            logger: Arc<MockLogger>,
            file_reader: Arc<MockFileReader>,
            links: Arc<MockLinks>,
            folder: Arc<MockFolder>,
        ) -> ActiveMountVersion {
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

            ActiveMountVersion::new(
                logger.clone(),
                token_validator.clone(),
                mount_path_factory.clone(),
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
                Arc::new(links),
                Arc::new(folder),
            )
            .perform("foo".to_string(), "bar".to_string(), "baz".to_string());

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

            log.expect_info().return_const(());
            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            folder
                .given_exists("/tmp/mount_root/bar/baz/current")
                .given_exists("/tmp/mount_root/bar/baz/v5");
            links.given_symlink(
                "/tmp/mount_root/bar/baz/current",
                "/tmp/mount_root/bar/baz/v5",
            );

            folder
                .expect_pop()
                .with(predicate::eq(Path::new("/tmp/mount_root/bar/baz/v5")))
                .return_const("v5".to_string());

            let actual = build(
                token_path,
                mount_root,
                Arc::new(log),
                Arc::new(file_reader),
                Arc::new(links),
                Arc::new(folder),
            )
            .perform("token".to_string(), "bar".to_string(), "baz".to_string());

            assert!(matches!(
                    actual,
                    Response::MountSet {
                        name,
                        version,
                        path
                    } if name == "baz" && version == Version(5) && path == Path::new("/tmp/mount_root/bar/baz/current")));
        }
    }
}
