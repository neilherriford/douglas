use super::ClientErrorDisplay;
use crate::encoding::safe_file_system_name;
use crate::version::{Version, VersionParseError};
use file_system::{FileSystemError, Folder, Links};
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub(super) enum MountPathVersionError {
    #[error("IO error determining version: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Version name parse error: {0}")]
    VersionParseError(#[from] VersionParseError),
    #[error("Could not determine version {0}")]
    CouldNotDetermineVersion(PathBuf),
    #[error("Invalid path {0}")]
    InvalidPath(PathBuf),
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
}

impl ClientErrorDisplay for MountPathVersionError {
    fn to_client_string(&self) -> String {
        "Could not determine mount path".to_string()
    }
}

pub(super) struct MountPathFactory {
    root: PathBuf,
    links: Arc<dyn Links + Sync + Send + 'static>,
    folder: Arc<dyn Folder + Sync + Send + 'static>,
}

impl MountPathFactory {
    pub fn new(
        root: &Path,
        folder: Arc<dyn Folder + Sync + Send + 'static>,
        links: Arc<dyn Links + Sync + Send + 'static>,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            folder,
            links,
        }
    }

    pub fn root_path(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn version_path(&self, service_name: &str, mount_name: &str, version: Version) -> PathBuf {
        let root = self.mount_path(service_name, mount_name);
        let mut result = root.to_path_buf();
        result.push(version.to_string());
        result
    }

    pub fn service_path(&self, service_name: &str) -> PathBuf {
        let service_name = safe_file_system_name(service_name);
        let mut result = self.root.clone();

        result.push(service_name);
        result
    }

    pub fn mount_path(&self, service_name: &str, mount_name: &str) -> PathBuf {
        let mut result = self.service_path(service_name);
        let mount_name = safe_file_system_name(mount_name);

        result.push(mount_name);

        result
    }

    pub fn active_version_path(&self, service_name: &str, mount_name: &str) -> PathBuf {
        let service_mount_root = self.mount_path(service_name, mount_name);
        let mut result = service_mount_root.to_path_buf();
        result.push("current");
        result
    }

    pub fn active_version(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Version, MountPathVersionError> {
        let active_version_path = self.active_version_path(service_name, mount_name);

        if self.folder.exists(&active_version_path) {
            let version_path = self.links.read(&active_version_path.as_path())?;
            self.derive_version_from_path(&version_path.as_path())
        } else {
            Err(MountPathVersionError::InvalidPath(active_version_path))
        }
    }

    fn derive_version_from_path(&self, path: &Path) -> Result<Version, MountPathVersionError> {
        if self.folder.exists(&path) {
            if let Some(version) = self.folder.pop(&path) {
                match version.parse::<Version>() {
                    Ok(version) => Ok(version),
                    Err(_) => Err(MountPathVersionError::CouldNotDetermineVersion(
                        path.to_path_buf(),
                    )),
                }
            } else {
                Err(MountPathVersionError::CouldNotDetermineVersion(
                    path.to_path_buf(),
                ))
            }
        } else {
            Err(MountPathVersionError::InvalidPath(path.to_path_buf()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MountPathFactory;
    use file_system::{MockFolder, MockLinks};
    use std::{path::Path, sync::Arc};

    fn build(root: &Path, folder: Arc<MockFolder>, links: Arc<MockLinks>) -> MountPathFactory {
        MountPathFactory::new(root, folder.clone(), links.clone())
    }

    mod service_mount_version_path {
        use super::build;
        use crate::Version;
        use file_system::{MockFolder, MockLinks};
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_create_with_escaped_names() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            let actual = build(&root, Arc::new(folder), Arc::new(links)).version_path(
                "foo service",
                "bar:mount",
                Version(5),
            );
            let expected = Path::new("/tmp/mount_root/foo%20service/bar%3Amount/v5").to_path_buf();

            assert_eq!(expected, actual)
        }
    }

    mod service_mount_root_path {
        use super::build;
        use file_system::{MockFolder, MockLinks};
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_create_with_escaped_names() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .mount_path("foo service", "bar:mount");
            let expected = Path::new("/tmp/mount_root/foo%20service/bar%3Amount").to_path_buf();

            assert_eq!(expected, actual)
        }
    }

    mod service_root_path {
        use super::build;
        use file_system::{MockFolder, MockLinks};
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_create_with_escaped_names() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            let actual =
                build(&root, Arc::new(folder), Arc::new(links)).service_path("foo service");
            let expected = Path::new("/tmp/mount_root/foo%20service").to_path_buf();

            assert_eq!(expected, actual)
        }
    }

    mod service_mount_active_version_path {
        use super::build;
        use file_system::{MockFolder, MockLinks};
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_create_with_escaped_names() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version_path("foo service", "bar:mount");
            let expected =
                Path::new("/tmp/mount_root/foo%20service/bar%3Amount/current").to_path_buf();

            assert_eq!(expected, actual)
        }
    }

    mod active_version {
        use super::build;
        use crate::{Version, server::mount_path_factory::MountPathVersionError};
        use file_system::{FileSystemError, MockFolder, MockLinks};
        use mockall::predicate;
        use std::{path::Path, sync::Arc};

        #[test]
        fn should_err_if_mount_does_not_exist() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder.given_folder_does_not_exist("/tmp/mount_root/foo%20service/bar%3Amount/current");
            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(actual, Err(MountPathVersionError::InvalidPath(_))));
        }

        #[test]
        fn should_err_if_link_not_readable() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder.given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/current");

            links
                .expect_read()
                .with(predicate::eq(Path::new(
                    "/tmp/mount_root/foo%20service/bar%3Amount/current",
                )))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(
                actual,
                Err(MountPathVersionError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_err_if_link_points_to_nowhere() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/current")
                .given_folder_does_not_exist("/tmp/mount_root/foo%20service/bar%3Amount/oops");
            links.given_symlink(
                "/tmp/mount_root/foo%20service/bar%3Amount/current",
                "/tmp/mount_root/foo%20service/bar%3Amount/oops",
            );

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(actual, Err(MountPathVersionError::InvalidPath(_))));
        }

        #[test]
        fn should_err_if_the_version_folder_could_not_be_popped() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/current")
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/oops");

            folder
                .expect_pop()
                .with(predicate::eq(Path::new(
                    "/tmp/mount_root/foo%20service/bar%3Amount/oops",
                )))
                .return_const(None);

            links.given_symlink(
                "/tmp/mount_root/foo%20service/bar%3Amount/current",
                "/tmp/mount_root/foo%20service/bar%3Amount/oops",
            );

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(
                actual,
                Err(MountPathVersionError::CouldNotDetermineVersion(_))
            ));
        }

        #[test]
        fn should_err_if_the_version_folder_improperly_named() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/current")
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/oops");

            folder
                .expect_pop()
                .with(predicate::eq(Path::new(
                    "/tmp/mount_root/foo%20service/bar%3Amount/oops",
                )))
                .return_const(Some("oops".to_string()));

            links.given_symlink(
                "/tmp/mount_root/foo%20service/bar%3Amount/current",
                "/tmp/mount_root/foo%20service/bar%3Amount/oops",
            );

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(
                actual,
                Err(MountPathVersionError::CouldNotDetermineVersion(_))
            ));
        }

        #[test]
        fn should_deterime_active_version() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let root = Path::new("/tmp/mount_root");

            folder
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/current")
                .given_folder_exists("/tmp/mount_root/foo%20service/bar%3Amount/v5");

            folder
                .expect_pop()
                .with(predicate::eq(Path::new(
                    "/tmp/mount_root/foo%20service/bar%3Amount/v5",
                )))
                .return_const(Some("v5".to_string()));

            links.given_symlink(
                "/tmp/mount_root/foo%20service/bar%3Amount/current",
                "/tmp/mount_root/foo%20service/bar%3Amount/v5",
            );

            let actual = build(&root, Arc::new(folder), Arc::new(links))
                .active_version("foo service", "bar:mount");

            assert!(matches!(
                actual,
                Ok(version) if version == Version(5)
            ));
        }
    }
}
