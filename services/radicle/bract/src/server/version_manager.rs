use super::ClientErrorDisplay;
use super::mount_path_factory::{MountPathFactory, MountPathVersionError};
use crate::Mount;
use crate::Service;
use crate::encoding::safe_prefixed_credential_name;
use crate::version::Version;
use credentials::Credentials;
use file_system::{EntryKind, FileDeleter, FileSystemError, Folder, Links, Modes, Permissions};
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub(super) enum VersionManagerError {
    #[error("Version already exists: {0}")]
    VersionAlreadyExists(Version),
    #[error("Application user not created, credentails not created?")]
    UnknownUser,
    #[error("Application group not created, credentails not created?")]
    UnknownGroup,
    #[error("Invalid path {0}")]
    InvalidPath(PathBuf),
    #[error("IO error determining version: {0}")]
    IoError(#[from] std::io::Error),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Mount path version error {0}")]
    MountPathVersionError(#[from] MountPathVersionError),
}

impl ClientErrorDisplay for VersionManagerError {
    fn to_client_string(&self) -> String {
        match self {
            VersionManagerError::UnknownUser | VersionManagerError::UnknownGroup => {
                "Could not create mount, credentials need to be created first.".to_string()
            }
            VersionManagerError::VersionAlreadyExists(version) => {
                format!("Mount already exists at version {}", version).to_string()
            }
            VersionManagerError::MountPathVersionError(error) => {
                format!("Could not derive mount path {}", error).to_string()
            }
            _ => "Could not create mount.".to_string(),
        }
    }
}

pub(super) struct VersionManager {
    mount_paths: Arc<MountPathFactory>,
    folder: Arc<dyn Folder + Sync + Send + 'static>,
    links: Arc<dyn Links + Sync + Send + 'static>,
    file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
    permissions: Arc<dyn Permissions + Sync + Send + 'static>,
    credentials: Arc<dyn Credentials + Sync + Send + 'static>,
}

impl VersionManager {
    pub fn new(
        mount_path_factory: Arc<MountPathFactory>,
        folder: Arc<dyn Folder + Sync + Send + 'static>,
        links: Arc<dyn Links + Sync + Send + 'static>,
        file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
        permissions: Arc<dyn Permissions + Sync + Send + 'static>,
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
    ) -> Self {
        Self {
            mount_paths: mount_path_factory,
            folder,
            links,
            file_deleter,
            permissions,
            credentials,
        }
    }

    pub fn is_initialized(&self, service_name: &str, mount_name: &str) -> bool {
        self.folder.exists(
            self.mount_paths
                .mount_path(&service_name, &mount_name)
                .as_path(),
        )
    }

    pub fn create(
        &self,
        service_name: &str,
        mount_name: &str,
        version: Version,
    ) -> Result<PathBuf, VersionManagerError> {
        let (user_name, group_name) = safe_prefixed_credential_name(&service_name);
        self.assert_credentials(&user_name, &group_name)?;

        self.initialize_mount_root()?;
        self.create_service_path(
            &self.mount_paths.service_path(&service_name),
            &user_name,
            &group_name,
        )?;
        self.create_service_path(
            &self.mount_paths.mount_path(&service_name, &mount_name),
            &user_name,
            &group_name,
        )?;

        let version_path = self
            .mount_paths
            .version_path(service_name, mount_name, version);
        let created = self.create_service_path(&version_path, &user_name, &group_name)?;
        if !created {
            return Err(VersionManagerError::VersionAlreadyExists(version));
        }

        let active_path = self
            .mount_paths
            .active_version_path(service_name, mount_name);

        self.set_active_version(&service_name, &active_path, &version_path)
    }

    fn assert_credentials(
        &self,
        user_name: &str,
        group_name: &str,
    ) -> Result<(), VersionManagerError> {
        if !self.credentials.user_exists(user_name) {
            Err(VersionManagerError::UnknownUser)
        } else if !self.credentials.group_exists(group_name) {
            Err(VersionManagerError::UnknownGroup)
        } else {
            Ok(())
        }
    }

    pub fn activate(
        &self,
        service_name: &str,
        mount_name: &str,
        version: Version,
    ) -> Result<PathBuf, VersionManagerError> {
        let (user_name, group_name) = safe_prefixed_credential_name(&service_name);
        self.assert_credentials(&user_name, &group_name)?;

        let version_path = self
            .mount_paths
            .version_path(service_name, mount_name, version);

        let active_path = self
            .mount_paths
            .active_version_path(service_name, mount_name);

        self.set_active_version(
            &service_name,
            &active_path.as_path(),
            &version_path.as_path(),
        )
    }

    pub fn versions(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<Vec<Version>, VersionManagerError> {
        let root = self.mount_paths.mount_path(service_name, mount_name);

        if self.folder.exists(&root) {
            let mut result: Vec<Version> = self
                .folder
                .entries(&root)?
                .into_iter()
                .filter_map(|entry| {
                    if entry.kind == EntryKind::File {
                        return None;
                    }
                    let version = entry.name.parse::<Version>().ok()?;
                    Some(version)
                })
                .collect();
            result.sort();

            Ok(result)
        } else {
            Err(VersionManagerError::InvalidPath(root.to_path_buf()))
        }
    }

    fn initialize_mount_root(&self) -> Result<(), VersionManagerError> {
        let mount_root = self.mount_paths.root_path();
        let mount_root = mount_root.as_path();

        if !self.folder.exists(&mount_root) {
            self.folder.create_recursively(&mount_root)?;
            self.set_ownership(
                &mount_root,
                credentials::ROOT_USER_NAME,
                credentials::ROOT_GROUP_NAME,
                Modes::OwnerReadWrite,
            )?;
        }

        Ok(())
    }

    fn set_active_version(
        &self,
        service_name: &str,
        active_path: &Path,
        version_path: &Path,
    ) -> Result<PathBuf, VersionManagerError> {
        let (user_name, group_name) = safe_prefixed_credential_name(&service_name);

        if self.folder.exists(&active_path) {
            self.file_deleter.delete(&active_path)?;
        }

        self.links.create(&version_path, &active_path)?;
        self.set_ownership(
            &active_path,
            &user_name,
            &group_name,
            Modes::OwnerReadWriteGroupReadWrite,
        )?;

        Ok(active_path.to_path_buf())
    }

    fn set_ownership(
        &self,
        path: &Path,
        user_name: &str,
        group_name: &str,
        mode: Modes,
    ) -> Result<(), FileSystemError> {
        self.permissions
            .change_user_and_group_ownership(&path, user_name, group_name)?;
        self.permissions.change_mode(&path, &mode)
    }

    fn create_service_path(
        &self,
        path: &PathBuf,
        user_name: &str,
        group_name: &str,
    ) -> Result<bool, FileSystemError> {
        let path = path.as_path();
        if self.folder.exists(path) {
            return Ok(false);
        }

        self.folder.create_recursively(&path)?;
        self.set_ownership(
            &path,
            &user_name,
            &group_name,
            Modes::OwnerReadWriteGroupReadWrite,
        )?;
        Ok(true)
    }

    pub fn list(&self) -> Result<Vec<Service>, VersionManagerError> {
        let mut services = Vec::<Service>::new();

        for service in self
            .folder
            .entries(self.mount_paths.root_path().as_path())?
            .iter()
            .filter(|entry| entry.kind == EntryKind::Directory)
        {
            let mut mounts = Vec::<Mount>::new();

            for mount in self
                .folder
                .entries(&self.mount_paths.service_path(&service.name))?
                .iter()
                .filter(|entry| entry.kind == EntryKind::Directory)
            {
                let version = self
                    .mount_paths
                    .active_version(&service.name, &mount.name)?;
                let path = self
                    .mount_paths
                    .active_version_path(&service.name, &mount.name);

                mounts.push(Mount {
                    name: mount.name.to_string(),
                    path,
                    version,
                });
            }
            services.push(Service {
                name: service.name.to_string(),
                mounts,
            });
        }

        Ok(services)
    }
}

#[cfg(test)]
mod tests {
    use super::{MountPathFactory, VersionManager};
    use credentials::MockCredentials;
    use file_system::{MockFileDeleter, MockFolder, MockLinks, MockPermissions};
    use std::{path::Path, sync::Arc};

    fn create_version_manager(
        folder: MockFolder,
        links: MockLinks,
        file_deleter: MockFileDeleter,
        permissions: MockPermissions,
        credentials: MockCredentials,
        mount_root: &Path,
    ) -> VersionManager {
        let folder = Arc::new(folder);
        let links = Arc::new(links);
        let mount_path_factory = MountPathFactory::new(&mount_root, folder.clone(), links.clone());

        VersionManager::new(
            Arc::new(mount_path_factory),
            folder.clone(),
            links.clone(),
            Arc::new(file_deleter),
            Arc::new(permissions),
            Arc::new(credentials),
        )
    }

    mod create {
        use super::create_version_manager;
        use crate::Version;
        use crate::server::version_manager::VersionManagerError;
        use credentials::MockCredentials;
        use file_system::FileSystemError;
        use file_system::{MockFileDeleter, MockFolder, MockLinks, MockPermissions, Modes};
        use mockall::predicate;
        use std::path::Path;

        fn expect_create_root(folder: &mut MockFolder, permissions: &mut MockPermissions) {
            folder.expect_create_folder_recursively_with("/tmp/mount_root/");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mount_root/",
                "root",
                "root",
                Modes::OwnerReadWrite,
            );
        }

        fn expect_create_service_and_mount_root(
            folder: &mut MockFolder,
            permissions: &mut MockPermissions,
        ) {
            folder.expect_create_folder_recursively_with("/tmp/mount_root/foo");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mount_root/foo",
                "doug-foo",
                "doug-foo",
                Modes::OwnerReadWriteGroupReadWrite,
            );
            folder.expect_create_folder_recursively_with("/tmp/mount_root/foo/bar");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mount_root/foo/bar",
                "doug-foo",
                "doug-foo",
                Modes::OwnerReadWriteGroupReadWrite,
            );
        }

        fn expect_version_folder_created(
            folder: &mut MockFolder,
            permissions: &mut MockPermissions,
        ) {
            folder.expect_create_folder_recursively_with("/tmp/mount_root/foo/bar/v0");
            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mount_root/foo/bar/v0",
                "doug-foo",
                "doug-foo",
                Modes::OwnerReadWriteGroupReadWrite,
            );
        }

        fn expect_current_version_recreated(
            file_deleter: &mut MockFileDeleter,
            links: &mut MockLinks,
        ) {
            file_deleter.expect_file_to_be_deleted("/tmp/mount_root/foo/bar/current");
            links.expect_create_with(
                "/tmp/mount_root/foo/bar/v0",
                "/tmp/mount_root/foo/bar/current",
            );
        }

        fn expect_create_service_directory(
            folder: &mut MockFolder,
            permissions: &mut MockPermissions,
            path: &str,
        ) {
            let path = path.to_string();
            folder.expect_create_folder_recursively_with(&path.clone());
            permissions.expect_ownership_and_mode_to_be_set(
                &path.clone(),
                "doug-foo",
                "doug-foo",
                Modes::OwnerReadWriteGroupReadWrite,
            );
        }

        #[test]
        fn should_error_is_service_user_doesnt_exist() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_does_not_exist("doug-foo");

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(actual, Err(VersionManagerError::UnknownUser)));
        }

        #[test]
        fn should_error_is_service_group_doesnt_exist() {
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials
                .given_user_exists("doug-foo")
                .given_group_does_not_exist("doug-foo");

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(actual, Err(VersionManagerError::UnknownGroup)));
        }

        #[test]
        fn should_error_if_could_not_create_root() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder.given_does_not_exist("/tmp/mount_root");
            folder
                .expect_create_recursively()
                .with(predicate::eq(Path::new("/tmp/mount_root")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_could_not_set_root_permissions() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_does_not_exist("/tmp/mount_root")
                .expect_create_folder_recursively_with("/tmp/mount_root");
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root")),
                    predicate::eq("root"),
                    predicate::eq("root"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_could_not_set_root_mode() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_does_not_exist("/tmp/mount_root/foo")
                .given_does_not_exist("/tmp/mount_root/foo/bar")
                .expect_create_folder_recursively_with("/tmp/mount_root")
                .expect_create_folder_recursively_with("/tmp/mount_root/foo");

            permissions.expect_ownership_to_be_set("/tmp/mount_root/foo", "doug-foo", "doug-foo");
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_could_not_create_service_directory() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_does_not_exist("/tmp/mount_root/foo")
                .given_does_not_exist("/tmp/mount_root/foo/bar");

            expect_create_service_directory(&mut folder, &mut permissions, "/tmp/mount_root/foo");
            folder
                .expect_create_recursively()
                .with(predicate::eq(Path::new("/tmp/mount_root/foo/bar")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_could_not_set_service_directory_perms() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_does_not_exist("/tmp/mount_root/foo")
                .given_does_not_exist("/tmp/mount_root/foo/bar")
                .expect_create_folder_recursively_with("/tmp/mount_root/foo/bar");

            expect_create_service_directory(&mut folder, &mut permissions, "/tmp/mount_root/foo");
            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar")),
                    predicate::eq("doug-foo"),
                    predicate::eq("doug-foo"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_could_not_set_service_directory_mode() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_does_not_exist("/tmp/mount_root/foo")
                .given_does_not_exist("/tmp/mount_root/foo/bar")
                .expect_create_folder_recursively_with("/tmp/mount_root/foo/bar");

            expect_create_service_directory(&mut folder, &mut permissions, "/tmp/mount_root/foo");
            permissions.expect_ownership_to_be_set(
                "/tmp/mount_root/foo/bar",
                "doug-foo",
                "doug-foo",
            );
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_version_path_already_exists() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_exists("/tmp/mount_root/foo/bar/v0");

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(
                matches!(actual, Err(VersionManagerError::VersionAlreadyExists(version)) if version == Version(0))
            );
        }

        #[test]
        fn should_error_if_creating_version_path_fails() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root/")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");

            folder
                .expect_create_recursively()
                .with(predicate::eq(Path::new("/tmp/mount_root/foo/bar/v0")))
                .returning(|_| Err(file_system::FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_ownership_failed() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0")
                .expect_create_folder_recursively_with("/tmp/mount_root/foo/bar/v0");

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/v0")),
                    predicate::eq("doug-foo"),
                    predicate::eq("doug-foo"),
                )
                .returning(|_, _, _| Err(file_system::FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_mode_failed() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0")
                .expect_create_folder_recursively_with("/tmp/mount_root/foo/bar/v0");

            permissions.expect_ownership_to_be_set(
                "/tmp/mount_root/foo/bar/v0",
                "doug-foo",
                "doug-foo",
            );
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/v0")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(file_system::FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_delete_failed() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_exists("/tmp/mount_root/foo/bar/current")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");
            expect_version_folder_created(&mut folder, &mut permissions);

            file_deleter
                .expect_delete()
                .with(predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_link_failed() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_exists("/tmp/mount_root/foo/bar/current")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");
            expect_version_folder_created(&mut folder, &mut permissions);

            file_deleter.expect_file_to_be_deleted("/tmp/mount_root/foo/bar/current");

            links
                .expect_create()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/v0")),
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_current_permissions_failed() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_exists("/tmp/mount_root/foo/bar/current")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");
            expect_version_folder_created(&mut folder, &mut permissions);
            expect_current_version_recreated(&mut file_deleter, &mut links);

            permissions
                .expect_change_user_and_group_ownership()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                    predicate::eq("doug-foo"),
                    predicate::eq("doug-foo"),
                )
                .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_current_mode_failed() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_exists("/tmp/mount_root")
                .given_exists("/tmp/mount_root/foo")
                .given_exists("/tmp/mount_root/foo/bar")
                .given_exists("/tmp/mount_root/foo/bar/current")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");
            expect_version_folder_created(&mut folder, &mut permissions);
            expect_current_version_recreated(&mut file_deleter, &mut links);

            permissions.expect_ownership_to_be_set(
                "/tmp/mount_root/foo/bar/current",
                "doug-foo",
                "doug-foo",
            );
            permissions
                .expect_change_mode()
                .with(
                    predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                    predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                )
                .returning(|_, _| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_create() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            let mut credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            credentials.given_user_and_group_exist("doug-foo", "doug-foo");
            folder
                .given_does_not_exist("/tmp/mount_root")
                .given_does_not_exist("/tmp/mount_root/foo")
                .given_does_not_exist("/tmp/mount_root/foo/bar")
                .given_does_not_exist("/tmp/mount_root/foo/bar/current")
                .given_does_not_exist("/tmp/mount_root/foo/bar/v0");

            expect_create_root(&mut folder, &mut permissions);
            expect_create_service_and_mount_root(&mut folder, &mut permissions);
            expect_version_folder_created(&mut folder, &mut permissions);
            expect_current_version_recreated(&mut file_deleter, &mut links);

            permissions.expect_ownership_and_mode_to_be_set(
                "/tmp/mount_root/foo/bar/current",
                "doug-foo",
                "doug-foo",
                Modes::OwnerReadWriteGroupReadWrite,
            );

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .create("foo", "bar", Version(0));

            assert!(matches!(
                actual,
                Ok(path) if path == Path::new("/tmp/mount_root/foo/bar/current").to_path_buf()
            ));
        }

        mod activate {
            use super::{create_version_manager, expect_current_version_recreated};
            use crate::{Version, server::version_manager::VersionManagerError};
            use credentials::MockCredentials;
            use file_system::{
                FileSystemError, MockFileDeleter, MockFolder, MockLinks, MockPermissions, Modes,
            };
            use mockall::predicate;
            use std::path::Path;

            #[test]
            fn should_error_if_service_user_does_not_exist() {
                let folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_does_not_exist("doug-foo");

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(actual, Err(VersionManagerError::UnknownUser)));
            }

            #[test]
            fn should_error_if_service_group_does_not_exist() {
                let folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials
                    .given_user_exists("doug-foo")
                    .given_group_does_not_exist("doug-foo");

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(actual, Err(VersionManagerError::UnknownGroup)));
            }

            #[test]
            fn should_error_if_active_version_could_not_be_deleted() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let mut file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_and_group_exist("doug-foo", "doug-foo");
                folder.given_exists("/tmp/mount_root/foo/bar/current");
                file_deleter
                    .expect_delete()
                    .with(predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")))
                    .returning(|_| Err(FileSystemError::ExpectedFileError));

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(
                    actual,
                    Err(VersionManagerError::FileSystemError(_))
                ));
            }

            #[test]
            fn should_error_if_link_could_not_be_created() {
                let mut folder = MockFolder::new();
                let mut links = MockLinks::new();
                let mut file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_and_group_exist("doug-foo", "doug-foo");
                folder.given_exists("/tmp/mount_root/foo/bar/current");
                file_deleter.expect_file_to_be_deleted("/tmp/mount_root/foo/bar/current");

                links
                    .expect_create()
                    .with(
                        predicate::eq(Path::new("/tmp/mount_root/foo/bar/v0")),
                        predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                    )
                    .returning(|_, _| Err(FileSystemError::ExpectedFileError));

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(
                    actual,
                    Err(VersionManagerError::FileSystemError(_))
                ));
            }

            #[test]
            fn should_error_if_permissions_could_not_be_set() {
                let mut folder = MockFolder::new();
                let mut links = MockLinks::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_and_group_exist("doug-foo", "doug-foo");
                folder.given_exists("/tmp/mount_root/foo/bar/current");
                file_deleter.expect_file_to_be_deleted("/tmp/mount_root/foo/bar/current");
                links.expect_create_with(
                    "/tmp/mount_root/foo/bar/v0",
                    "/tmp/mount_root/foo/bar/current",
                );

                permissions
                    .expect_change_user_and_group_ownership()
                    .with(
                        predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                        predicate::eq("doug-foo"),
                        predicate::eq("doug-foo"),
                    )
                    .returning(|_, _, _| Err(FileSystemError::ExpectedFileError));

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(
                    actual,
                    Err(VersionManagerError::FileSystemError(_))
                ));
            }

            #[test]
            fn should_error_if_mode_could_not_be_set() {
                let mut folder = MockFolder::new();
                let mut links = MockLinks::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_and_group_exist("doug-foo", "doug-foo");
                folder
                    .given_exists("/tmp/mount_root")
                    .given_exists("/tmp/mount_root/foo")
                    .given_exists("/tmp/mount_root/foo/bar")
                    .given_exists("/tmp/mount_root/foo/bar/current");
                file_deleter.expect_file_to_be_deleted("/tmp/mount_root/foo/bar/current");
                links.expect_create_with(
                    "/tmp/mount_root/foo/bar/v0",
                    "/tmp/mount_root/foo/bar/current",
                );

                permissions.expect_ownership_to_be_set(
                    "/tmp/mount_root/foo/bar/current",
                    "doug-foo",
                    "doug-foo",
                );
                permissions
                    .expect_change_mode()
                    .with(
                        predicate::eq(Path::new("/tmp/mount_root/foo/bar/current")),
                        predicate::eq(Modes::OwnerReadWriteGroupReadWrite),
                    )
                    .returning(|_, _| Err(FileSystemError::ExpectedFileError));

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(matches!(
                    actual,
                    Err(VersionManagerError::FileSystemError(_))
                ));
            }

            #[test]
            fn should_set_active_version() {
                let mut folder = MockFolder::new();
                let mut links = MockLinks::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut permissions = MockPermissions::new();
                let mut credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                credentials.given_user_and_group_exist("doug-foo", "doug-foo");
                folder
                    .given_exists("/tmp/mount_root")
                    .given_exists("/tmp/mount_root/foo")
                    .given_exists("/tmp/mount_root/foo/bar")
                    .given_exists("/tmp/mount_root/foo/bar/current");
                expect_current_version_recreated(&mut file_deleter, &mut links);

                permissions.expect_ownership_and_mode_to_be_set(
                    "/tmp/mount_root/foo/bar/current",
                    "doug-foo",
                    "doug-foo",
                    Modes::OwnerReadWriteGroupReadWrite,
                );

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .activate("foo", "bar", Version(0));

                assert!(
                    matches!(actual, Ok(path) if path == Path::new("/tmp/mount_root/foo/bar/current").to_path_buf())
                );
            }
        }

        mod is_initialized {
            use super::super::create_version_manager;
            use credentials::MockCredentials;
            use file_system::{MockFileDeleter, MockFolder, MockLinks, MockPermissions};
            use std::path::Path;

            #[test]
            fn should_consider_missing_directory_as_non_initailized() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder.given_does_not_exist("/tmp/mount_root/foo/bar");

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .is_initialized("foo", "bar");

                assert_eq!(false, actual);
            }

            #[test]
            fn should_consider_present_directory_as_initailized() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder.given_exists("/tmp/mount_root/foo/bar");

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .is_initialized("foo", "bar");

                assert_eq!(true, actual);
            }
        }

        mod versions {
            use super::create_version_manager;
            use crate::{Version, server::version_manager::VersionManagerError};
            use credentials::MockCredentials;
            use file_system::{Entry, MockFileDeleter, MockFolder, MockLinks, MockPermissions};
            use std::path::Path;

            #[test]
            fn should_err_if_root_does_not_exist() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder.given_does_not_exist("/tmp/mount_root/foo%20service/bar%3Amount");

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .versions("foo service", "bar:mount");

                assert!(matches!(actual, Err(VersionManagerError::InvalidPath(_))));
            }

            #[test]
            fn should_ignore_files() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder
                    .given_exists("/tmp/mount_root/foo%20service/bar%3Amount")
                    .given_folder_entries(
                        "/tmp/mount_root/foo%20service/bar%3Amount",
                        vec![
                            Entry::create_file_entry("foo"),
                            Entry::create_file_entry("bar"),
                        ],
                    );

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .versions("foo service", "bar:mount");

                assert!(matches!(actual, Ok(versions) if versions == vec![]));
            }

            #[test]
            fn should_ignore_non_version_folders() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder
                    .given_exists("/tmp/mount_root/foo%20service/bar%3Amount")
                    .given_folder_entries(
                        "/tmp/mount_root/foo%20service/bar%3Amount",
                        vec![
                            Entry::create_file_entry("foo"),
                            Entry::create_file_entry("bar"),
                            Entry::create_directory("baz"),
                        ],
                    );

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .versions("foo service", "bar:mount");

                assert!(matches!(actual, Ok(versions) if versions == vec![]));
            }

            #[test]
            fn should_return_version_named_folders() {
                let mut folder = MockFolder::new();
                let links = MockLinks::new();
                let file_deleter = MockFileDeleter::new();
                let permissions = MockPermissions::new();
                let credentials = MockCredentials::new();
                let mount_root = Path::new("/tmp/mount_root");

                folder
                    .given_exists("/tmp/mount_root/foo%20service/bar%3Amount")
                    .given_folder_entries(
                        "/tmp/mount_root/foo%20service/bar%3Amount",
                        vec![
                            Entry::create_file_entry("foo"),
                            Entry::create_file_entry("bar"),
                            Entry::create_directory("baz"),
                            Entry::create_directory("v0"),
                            Entry::create_directory("v1"),
                        ],
                    );

                let actual = create_version_manager(
                    folder,
                    links,
                    file_deleter,
                    permissions,
                    credentials,
                    &mount_root,
                )
                .versions("foo service", "bar:mount");

                assert!(matches!(actual, Ok(versions) if versions == vec![Version(0), Version(1)]));
            }
        }
    }

    mod list {
        use super::create_version_manager;
        use crate::Mount;
        use crate::{Service, Version, server::version_manager::VersionManagerError};
        use credentials::MockCredentials;
        use file_system::{
            Entry, FileSystemError, MockFileDeleter, MockFolder, MockLinks, MockPermissions,
        };
        use mockall::predicate;
        use std::path::Path;

        #[test]
        fn should_error_if_services_not_enumerable() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            folder
                .expect_entries()
                .with(predicate::eq(Path::new("/tmp/mount_root")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .list();

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_mounts_not_enumerable() {
            let mut folder = MockFolder::new();
            let links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            folder.given_folder_entries(
                "/tmp/mount_root",
                vec![Entry::create_directory("foo-service")],
            );

            folder
                .expect_entries()
                .with(predicate::eq(Path::new("/tmp/mount_root/foo-service")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .list();

            assert!(matches!(
                actual,
                Err(VersionManagerError::FileSystemError(_))
            ));
        }

        #[test]
        fn should_error_if_versions_could_not_be_retrieved() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            folder
                .given_exists("/tmp/mount_root")
                .given_folder_entries(
                    "/tmp/mount_root",
                    vec![Entry::create_directory("foo-service")],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount")
                .given_folder_entries(
                    "/tmp/mount_root/foo-service",
                    vec![Entry::create_directory("bar-mount")],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount/current")
                .given_does_not_exist("/tmp/mount_root/foo-service/bar-mount/oops");

            links.given_symlink(
                "/tmp/mount_root/foo-service/bar-mount/current",
                "/tmp/mount_root/foo-service/bar-mount/oops",
            );

            folder
                .expect_entries()
                .with(predicate::eq(Path::new(
                    "/tmp/mount_root/foo-service/bar-mount",
                )))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .list();

            assert!(matches!(
                actual,
                Err(VersionManagerError::MountPathVersionError(_))
            ));
        }

        #[test]
        fn should_list() {
            let mut folder = MockFolder::new();
            let mut links = MockLinks::new();
            let file_deleter = MockFileDeleter::new();
            let permissions = MockPermissions::new();
            let credentials = MockCredentials::new();
            let mount_root = Path::new("/tmp/mount_root");

            folder
                .given_exists("/tmp/mount_root")
                .given_folder_entries(
                    "/tmp/mount_root",
                    vec![Entry::create_directory("foo-service")],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount")
                .given_folder_entries(
                    "/tmp/mount_root/foo-service",
                    vec![Entry::create_directory("bar-mount")],
                )
                .given_folder_entries(
                    "/tmp/mount_root/foo-service/bar-mount",
                    vec![
                        Entry::create_directory("current"),
                        Entry::create_directory("v0"),
                    ],
                )
                .given_exists("/tmp/mount_root/foo-service/bar-mount/current")
                .given_exists("/tmp/mount_root/foo-service/bar-mount/v0")
                .expect_pop_with("/tmp/mount_root/foo-service/bar-mount/v0", "v0");

            links.given_symlink(
                "/tmp/mount_root/foo-service/bar-mount/current",
                "/tmp/mount_root/foo-service/bar-mount/v0",
            );

            let actual = create_version_manager(
                folder,
                links,
                file_deleter,
                permissions,
                credentials,
                &mount_root,
            )
            .list();

            let expected = vec![Service {
                mounts: vec![Mount {
                    name: "bar-mount".to_string(),
                    version: Version(0),
                    path: Path::new("/tmp/mount_root/foo-service/bar-mount/current").to_path_buf(),
                }],
                name: "foo-service".to_string(),
            }];

            assert!(actual.is_ok());
            assert_eq!(actual.unwrap(), expected);
        }
    }
}
