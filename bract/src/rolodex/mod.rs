mod naming;

use credentials::{Credentials, CredentialsError};
use file_system::{FileReader, FileSystemError, FileWriter, Folder};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

const MAX_ATTEMPTS: u32 = 16;

#[derive(Debug, Error)]
pub enum RolodexError {
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("Could not find a free system name for '{0}'")]
    ExhaustedNameSpace(String),
    #[error("Unknown guest '{0}': it has no reserved user yet")]
    UnknownGuest(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub id: u32,
    pub full_name: String,
    pub system_name: String,
}

pub struct ServiceAccount {
    pub user: Credential,
    pub group: Credential,
}

#[cfg_attr(test, mockall::automock)]
pub trait Rolodex: Send + Sync {
    fn find_service_account(&self, full_name: &str)
    -> Result<Option<ServiceAccount>, RolodexError>;
    fn create_service_account(&self, full_name: &str) -> Result<ServiceAccount, RolodexError>;
    fn find_share_group(
        &self,
        owner_full_name: &str,
        share_name: &str,
    ) -> Result<Option<Credential>, RolodexError>;
    fn create_share_group(
        &self,
        owner_full_name: &str,
        share_name: &str,
        guest_full_names: &[String],
    ) -> Result<Credential, RolodexError>;
}

pub struct FileRolodex {
    root: PathBuf,
    credentials: Arc<dyn Credentials>,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
}

impl FileRolodex {
    pub fn new(
        root: PathBuf,
        credentials: Arc<dyn Credentials>,
        folder: Arc<dyn Folder>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
    ) -> Self {
        Self {
            root,
            credentials,
            folder,
            file_reader,
            file_writer,
        }
    }

    fn users_root(&self) -> PathBuf {
        self.root.join("users")
    }

    fn groups_root(&self) -> PathBuf {
        self.root.join("groups")
    }

    fn find_group(&self, full_name: &str) -> Result<Option<Credential>, RolodexError> {
        let Some(system_name) = self.locate(&self.groups_root(), full_name, false)? else {
            return Ok(None);
        };

        if !self.credentials.group_exists(&system_name) {
            return Ok(None);
        }

        let id = self.credentials.get_group_id(&system_name).ok_or_else(|| {
            CredentialsError::GroupNotFoundError {
                name: system_name.clone(),
            }
        })?;

        Ok(Some(Credential {
            id,
            full_name: full_name.to_string(),
            system_name,
        }))
    }

    fn find_user(&self, full_name: &str) -> Result<Option<Credential>, RolodexError> {
        let Some(system_name) = self.locate(&self.users_root(), full_name, false)? else {
            return Ok(None);
        };

        if !self.credentials.user_exists(&system_name) {
            return Ok(None);
        }

        let id = self.credentials.get_user_id(&system_name).ok_or_else(|| {
            CredentialsError::UserNotFoundError {
                name: system_name.clone(),
            }
        })?;

        Ok(Some(Credential {
            id,
            full_name: full_name.to_string(),
            system_name,
        }))
    }

    fn get_or_create_group(&self, full_name: &str) -> Result<Credential, RolodexError> {
        let system_name = self.reserve(&self.groups_root(), full_name)?;

        let id = if self.credentials.group_exists(&system_name) {
            self.credentials.get_group_id(&system_name).ok_or_else(|| {
                CredentialsError::GroupNotFoundError {
                    name: system_name.clone(),
                }
            })?
        } else {
            self.credentials.create_group(&system_name)?
        };

        Ok(Credential {
            id,
            full_name: full_name.to_string(),
            system_name,
        })
    }

    fn get_or_create_user(
        &self,
        full_name: &str,
        primary_group_system_name: &str,
        additional_group_names: Vec<String>,
    ) -> Result<Credential, RolodexError> {
        let system_name = self.reserve(&self.users_root(), full_name)?;

        let id = if self.credentials.user_exists(&system_name) {
            let id = self.credentials.get_user_id(&system_name).ok_or_else(|| {
                CredentialsError::UserNotFoundError {
                    name: system_name.clone(),
                }
            })?;

            self.credentials
                .set_primary_group(&system_name, primary_group_system_name)?;
            for group_name in &additional_group_names {
                self.credentials.join_group(&system_name, group_name)?;
            }

            id
        } else {
            self.credentials.create_user(
                &system_name,
                primary_group_system_name,
                additional_group_names,
            )?
        };

        Ok(Credential {
            id,
            full_name: full_name.to_string(),
            system_name,
        })
    }

    fn reserve(&self, kind_root: &Path, full_name: &str) -> Result<String, RolodexError> {
        self.locate(kind_root, full_name, true)?
            .ok_or_else(|| RolodexError::ExhaustedNameSpace(full_name.to_string()))
    }

    fn locate(
        &self,
        kind_root: &Path,
        full_name: &str,
        create_if_missing: bool,
    ) -> Result<Option<String>, RolodexError> {
        if create_if_missing {
            self.folder.create_recursively(kind_root)?;
        }

        for attempt in 0..MAX_ATTEMPTS {
            let system_name = naming::derive_system_name(&naming::salted(full_name, attempt));
            let record_path = kind_root.join(&system_name);

            if self.file_reader.exists(&record_path) {
                let existing_full_name = self.file_reader.read_all(&record_path)?;
                if existing_full_name == full_name {
                    return Ok(Some(system_name));
                }
                continue;
            }

            if create_if_missing {
                self.file_writer.write_all(&record_path, full_name)?;
                return Ok(Some(system_name));
            }

            return Ok(None);
        }

        Err(RolodexError::ExhaustedNameSpace(full_name.to_string()))
    }
}

impl Rolodex for FileRolodex {
    fn find_service_account(
        &self,
        full_name: &str,
    ) -> Result<Option<ServiceAccount>, RolodexError> {
        let Some(group) = self.find_group(full_name)? else {
            return Ok(None);
        };
        let Some(user) = self.find_user(full_name)? else {
            return Ok(None);
        };

        Ok(Some(ServiceAccount { user, group }))
    }

    fn create_service_account(&self, full_name: &str) -> Result<ServiceAccount, RolodexError> {
        let group = self.get_or_create_group(full_name)?;
        let user = self.get_or_create_user(full_name, &group.system_name, Vec::new())?;

        Ok(ServiceAccount { user, group })
    }

    fn find_share_group(
        &self,
        owner_full_name: &str,
        share_name: &str,
    ) -> Result<Option<Credential>, RolodexError> {
        self.find_group(&format!("{owner_full_name}:{share_name}"))
    }

    fn create_share_group(
        &self,
        owner_full_name: &str,
        share_name: &str,
        guest_full_names: &[String],
    ) -> Result<Credential, RolodexError> {
        let share_full_name = format!("{owner_full_name}:{share_name}");
        let group = self.get_or_create_group(&share_full_name)?;

        for guest_full_name in guest_full_names {
            let guest_user_system_name =
                self.locate(&self.users_root(), guest_full_name, false)?
                    .ok_or_else(|| RolodexError::UnknownGuest(guest_full_name.clone()))?;

            self.credentials
                .join_group(&guest_user_system_name, &group.system_name)?;
        }

        Ok(group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials::MockCredentials;
    use file_system::{MockFileReader, MockFileWriter, MockFolder};

    fn rolodex(
        credentials: MockCredentials,
        folder: MockFolder,
        file_reader: MockFileReader,
        file_writer: MockFileWriter,
    ) -> FileRolodex {
        FileRolodex::new(
            PathBuf::from("/var/lib/douglas/bract/rolodex"),
            Arc::new(credentials),
            Arc::new(folder),
            Arc::new(file_reader),
            Arc::new(file_writer),
        )
    }

    fn group_system_name() -> String {
        naming::derive_system_name("traefik")
    }

    mod find_service_account {
        use super::*;

        #[test]
        fn test_should_return_none_when_the_group_has_never_been_reserved() {
            let group_path = format!(
                "/var/lib/douglas/bract/rolodex/groups/{}",
                group_system_name()
            );

            let mut file_reader = MockFileReader::new();
            file_reader.given_does_not_exist(&group_path);

            let rolodex = rolodex(
                MockCredentials::new(),
                MockFolder::new(),
                file_reader,
                MockFileWriter::new(),
            );

            let result = rolodex
                .find_service_account("traefik")
                .expect("should look up");

            assert!(result.is_none());
        }

        #[test]
        fn test_should_return_none_when_reserved_but_the_os_group_was_never_created() {
            let system_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{system_name}");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_exists(&group_path)
                .given_can_read_all_with_contents(&group_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&system_name);

            let rolodex = rolodex(
                credentials,
                MockFolder::new(),
                file_reader,
                MockFileWriter::new(),
            );

            let result = rolodex
                .find_service_account("traefik")
                .expect("should look up");

            assert!(result.is_none());
        }

        #[test]
        fn test_should_return_the_service_account_when_fully_provisioned() {
            let system_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{system_name}");
            let user_path = format!("/var/lib/douglas/bract/rolodex/users/{system_name}");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_exists(&group_path)
                .given_can_read_all_with_contents(&group_path, "traefik")
                .given_exists(&user_path)
                .given_can_read_all_with_contents(&user_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_exists_with_gid(&system_name, 42);
            credentials.given_user_exists(&system_name);
            credentials.expect_get_user_id().returning(|_| Some(7));

            let rolodex = rolodex(
                credentials,
                MockFolder::new(),
                file_reader,
                MockFileWriter::new(),
            );

            let account = rolodex
                .find_service_account("traefik")
                .expect("should look up")
                .expect("should be found");

            assert_eq!(account.group.id, 42);
            assert_eq!(account.user.id, 7);
        }

        #[test]
        fn test_should_never_create_the_rolodex_directories() {
            let folder = MockFolder::new();
            // No expectations set on `folder`. If `find_service_account` ever
            // called `create_recursively`, this mock would panic on the
            // unexpected call, failing the test.

            let mut file_reader = MockFileReader::new();
            file_reader.given_does_not_exist(&format!(
                "/var/lib/douglas/bract/rolodex/groups/{}",
                group_system_name()
            ));

            let rolodex = rolodex(
                MockCredentials::new(),
                folder,
                file_reader,
                MockFileWriter::new(),
            );

            let result = rolodex
                .find_service_account("traefik")
                .expect("should look up");

            assert!(result.is_none());
        }
    }

    mod find_share_group {
        use super::*;

        #[test]
        fn test_should_return_none_when_the_share_group_does_not_exist() {
            let share_system_name = naming::derive_system_name("traefik:config");
            let share_group_path =
                format!("/var/lib/douglas/bract/rolodex/groups/{share_system_name}");

            let mut file_reader = MockFileReader::new();
            file_reader.given_does_not_exist(&share_group_path);

            let rolodex = rolodex(
                MockCredentials::new(),
                MockFolder::new(),
                file_reader,
                MockFileWriter::new(),
            );

            let result = rolodex
                .find_share_group("traefik", "config")
                .expect("should look up");

            assert!(result.is_none());
        }

        #[test]
        fn test_should_return_the_share_group_when_it_exists() {
            let share_full_name = "traefik:config".to_string();
            let share_system_name = naming::derive_system_name(&share_full_name);
            let share_group_path =
                format!("/var/lib/douglas/bract/rolodex/groups/{share_system_name}");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_exists(&share_group_path)
                .given_can_read_all_with_contents(&share_group_path, &share_full_name);

            let mut credentials = MockCredentials::new();
            credentials.given_group_exists_with_gid(&share_system_name, 99);

            let rolodex = rolodex(
                credentials,
                MockFolder::new(),
                file_reader,
                MockFileWriter::new(),
            );

            let group = rolodex
                .find_share_group("traefik", "config")
                .expect("should look up")
                .expect("should be found");

            assert_eq!(group.id, 99);
            assert_eq!(group.system_name, share_system_name);
        }
    }

    mod create_service_account {
        use super::*;

        #[test]
        fn test_should_create_a_new_group_and_user_when_neither_exists() {
            let system_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{system_name}");
            let user_path = format!("/var/lib/douglas/bract/rolodex/users/{system_name}");

            let mut folder = MockFolder::new();
            folder
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups")
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/users");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_does_not_exist(&group_path)
                .given_does_not_exist(&user_path);

            let mut file_writer = MockFileWriter::new();
            file_writer
                .expect_write_to_file_with_contents(&group_path, "traefik")
                .expect_write_to_file_with_contents(&user_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&system_name);
            credentials
                .expect_create_group()
                .withf({
                    let system_name = system_name.clone();
                    move |name| name == system_name
                })
                .returning(|_| Ok(42));
            credentials.given_user_does_not_exist(&system_name);
            credentials
                .expect_create_user()
                .withf({
                    let system_name = system_name.clone();
                    move |name, primary_group, _| {
                        name == system_name && primary_group == system_name
                    }
                })
                .returning(|_, _, _| Ok(7));

            let rolodex = rolodex(credentials, folder, file_reader, file_writer);
            let account = rolodex
                .create_service_account("traefik")
                .expect("should create service account");

            assert_eq!(account.group.id, 42);
            assert_eq!(account.group.system_name, system_name);
            assert_eq!(account.user.id, 7);
            assert_eq!(account.user.system_name, system_name);
        }

        #[test]
        fn test_should_reuse_the_existing_credentials_when_already_reserved() {
            let system_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{system_name}");
            let user_path = format!("/var/lib/douglas/bract/rolodex/users/{system_name}");

            let mut folder = MockFolder::new();
            folder
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups")
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/users");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_exists(&group_path)
                .given_can_read_all_with_contents(&group_path, "traefik")
                .given_exists(&user_path)
                .given_can_read_all_with_contents(&user_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_exists_with_gid(&system_name, 42);
            credentials.given_user_exists(&system_name);
            credentials.expect_get_user_id().returning(|_| Some(7));
            credentials
                .expect_set_primary_group()
                .withf({
                    let system_name = system_name.clone();
                    move |user, group| user == system_name && group == system_name
                })
                .returning(|_, _| Ok(42));

            let rolodex = rolodex(credentials, folder, file_reader, MockFileWriter::new());
            let account = rolodex
                .create_service_account("traefik")
                .expect("should reuse existing service account");

            assert_eq!(account.group.id, 42);
            assert_eq!(account.user.id, 7);
        }

        #[test]
        fn test_should_repin_the_users_primary_group_when_only_the_user_survived() {
            let system_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{system_name}");
            let user_path = format!("/var/lib/douglas/bract/rolodex/users/{system_name}");

            let mut folder = MockFolder::new();
            folder
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups")
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/users");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_does_not_exist(&group_path)
                .given_exists(&user_path)
                .given_can_read_all_with_contents(&user_path, "traefik");

            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_to_file_with_contents(&group_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&system_name);
            credentials.expect_create_group().returning(|_| Ok(42));
            credentials.given_user_exists(&system_name);
            credentials.expect_get_user_id().returning(|_| Some(7));
            credentials
                .expect_set_primary_group()
                .withf({
                    let system_name = system_name.clone();
                    move |user, group| user == system_name && group == system_name
                })
                .returning(|_, _| Ok(42));

            let rolodex = rolodex(credentials, folder, file_reader, file_writer);
            let account = rolodex
                .create_service_account("traefik")
                .expect("should heal the stale primary group link");

            assert_eq!(account.group.id, 42);
            assert_eq!(account.user.id, 7);
        }

        #[test]
        fn test_should_retry_with_a_salted_name_on_a_genuine_collision() {
            let colliding_name = group_system_name();
            let group_path = format!("/var/lib/douglas/bract/rolodex/groups/{colliding_name}");
            let retried_name = naming::derive_system_name(&naming::salted("traefik", 1));
            let retried_path = format!("/var/lib/douglas/bract/rolodex/groups/{retried_name}");
            let user_path = format!("/var/lib/douglas/bract/rolodex/users/{colliding_name}");

            let mut folder = MockFolder::new();
            folder
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups")
                .expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/users");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_exists(&group_path)
                .given_can_read_all_with_contents(&group_path, "some-other-service")
                .given_does_not_exist(&retried_path)
                .given_does_not_exist(&user_path);

            let mut file_writer = MockFileWriter::new();
            file_writer
                .expect_write_to_file_with_contents(&retried_path, "traefik")
                .expect_write_to_file_with_contents(&user_path, "traefik");

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&retried_name);
            credentials
                .expect_create_group()
                .withf({
                    let retried_name = retried_name.clone();
                    move |name| name == retried_name
                })
                .returning(|_| Ok(43));
            credentials.given_user_does_not_exist(&colliding_name);
            credentials.expect_create_user().returning(|_, _, _| Ok(8));

            let rolodex = rolodex(credentials, folder, file_reader, file_writer);
            let account = rolodex
                .create_service_account("traefik")
                .expect("should retry past the collision");

            assert_eq!(account.group.system_name, retried_name);
        }
    }

    mod create_share_group {
        use super::*;

        #[test]
        fn test_should_join_each_guest_users_reserved_system_name() {
            let share_full_name = "traefik:config".to_string();
            let share_system_name = naming::derive_system_name(&share_full_name);
            let share_group_path =
                format!("/var/lib/douglas/bract/rolodex/groups/{share_system_name}");

            let guest_system_name = naming::derive_system_name("openbao");
            let guest_user_path =
                format!("/var/lib/douglas/bract/rolodex/users/{guest_system_name}");

            let mut folder = MockFolder::new();
            folder.expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_does_not_exist(&share_group_path)
                .given_exists(&guest_user_path)
                .given_can_read_all_with_contents(&guest_user_path, "openbao");

            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_to_file_with_contents(&share_group_path, &share_full_name);

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&share_system_name);
            credentials.expect_create_group().returning(|_| Ok(99));
            credentials
                .expect_join_group()
                .withf({
                    let guest_system_name = guest_system_name.clone();
                    let share_system_name = share_system_name.clone();
                    move |user, group| user == guest_system_name && group == share_system_name
                })
                .returning(|_, _| Ok(()));

            let rolodex = rolodex(credentials, folder, file_reader, file_writer);
            let group = rolodex
                .create_share_group("traefik", "config", &["openbao".to_string()])
                .expect("should create share group and join guest");

            assert_eq!(group.system_name, share_system_name);
        }

        #[test]
        fn test_should_fail_when_a_guest_has_no_reserved_user_yet() {
            let share_full_name = "traefik:config".to_string();
            let share_system_name = naming::derive_system_name(&share_full_name);
            let share_group_path =
                format!("/var/lib/douglas/bract/rolodex/groups/{share_system_name}");

            let guest_system_name = naming::derive_system_name("openbao");
            let guest_user_path =
                format!("/var/lib/douglas/bract/rolodex/users/{guest_system_name}");

            let mut folder = MockFolder::new();
            folder.expect_create_folder_recursively_with("/var/lib/douglas/bract/rolodex/groups");

            let mut file_reader = MockFileReader::new();
            file_reader
                .given_does_not_exist(&share_group_path)
                .given_does_not_exist(&guest_user_path);

            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_to_file_with_contents(&share_group_path, &share_full_name);

            let mut credentials = MockCredentials::new();
            credentials.given_group_does_not_exist(&share_system_name);
            credentials.expect_create_group().returning(|_| Ok(99));

            let rolodex = rolodex(credentials, folder, file_reader, file_writer);
            let result = rolodex.create_share_group("traefik", "config", &["openbao".to_string()]);

            assert!(matches!(result, Err(RolodexError::UnknownGuest(guest)) if guest == "openbao"));
        }
    }
}
