mod linux_credentials;
mod macos_credentials;
mod queries;

#[cfg(target_os = "linux")]
use crate::linux_credentials::LinuxCredentials;
#[cfg(target_os = "macos")]
use crate::macos_credentials::MacOSCredentials;

#[cfg(feature = "mock")]
use mockall::{automock, predicate};

use os::{Os, OsError};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CredentialsError {
    #[error("The name could not be converted to unicode")]
    InvalidName,
    #[error("Could not find user '{name}'")]
    UserNotFoundError { name: String },
    #[error("Could not find group '{name}'")]
    GroupNotFoundError { name: String },
    #[error("OS error '{0}'")]
    IoError(#[from] OsError),
    #[error("General error '{0}'")]
    GeneralError(String),
}

pub static ROOT_USER_NAME: &str = "root";
pub static ROOT_GROUP_NAME: &str = "root";

#[cfg_attr(feature = "mock", automock)]
pub trait Credentials {
    fn is_root(&self) -> bool;
    fn create_user(
        &self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<String>,
    ) -> Result<(), CredentialsError>;
    fn user_exists(&self, name: &str) -> bool;
    fn delete_user(&self, name: &str) -> Result<(), CredentialsError>;
    fn create_group(&self, name: &str) -> Result<(), CredentialsError>;
    fn group_exists(&self, name: &str) -> bool;
    fn group_memberships(&self, name: &str) -> Vec<String>;
    fn get_group_id(&self, name: &str) -> Option<u32>;
    fn delete_group(&self, name: &str) -> Result<(), CredentialsError>;
    fn list_users(&self) -> Result<Vec<String>, CredentialsError>;
    fn join_group(&self, user_name: &str, group_name: &str) -> Result<(), CredentialsError>;
}

#[cfg(feature = "mock")]
impl MockCredentials {
    pub fn given_user(&mut self, user_name: &str, exists: bool) -> &mut Self {
        let user_name = user_name.to_string();
        self.expect_user_exists()
            .with(predicate::eq(user_name.clone()))
            .return_const(exists);

        self
    }

    pub fn given_user_exists(&mut self, user_name: &str) -> &mut Self {
        self.given_user(user_name, true)
    }

    pub fn given_user_does_not_exist(&mut self, user_name: &str) -> &mut Self {
        self.given_user(user_name, false)
    }

    pub fn given_group(&mut self, group_name: &str, exists: bool) -> &mut Self {
        let group_name = group_name.to_string();
        self.expect_group_exists()
            .with(predicate::eq(group_name.clone()))
            .return_const(exists);

        self
    }

    pub fn given_group_exists(&mut self, group_name: &str) -> &mut Self {
        self.given_group(group_name, true)
    }

    pub fn given_group_does_not_exist(&mut self, group_name: &str) -> &mut Self {
        self.given_group(group_name, false)
    }

    pub fn given_user_and_group_exist(&mut self, user_name: &str, group_name: &str) -> &mut Self {
        self.given_user_exists(user_name)
            .given_group_exists(group_name)
    }

    pub fn given_user_and_group_do_not_exist(
        &mut self,
        user_name: &str,
        group_name: &str,
    ) -> &mut Self {
        self.given_user_does_not_exist(user_name)
            .given_group_does_not_exist(group_name)
    }

    pub fn given_is_root(&mut self) -> &mut Self {
        self.expect_is_root().return_const(true);
        self
    }

    pub fn given_is_not_root(&mut self) -> &mut Self {
        self.expect_is_root().return_const(false);
        self
    }

    pub fn expect_group_created_named(&mut self, name: &str) -> &mut Self {
        let name = name.to_string();
        self.expect_create_group()
            .with(predicate::eq(name))
            .returning(|_| Ok(()));
        self
    }

    pub fn expect_user_created_named(
        &mut self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<&str>,
    ) -> &mut Self {
        let name = name.to_string();
        let primary_group = primary_group_name.to_string();
        let group_names: Vec<String> = group_names.iter().map(|name| name.to_string()).collect();

        self.expect_create_user()
            .with(
                predicate::eq(name),
                predicate::eq(primary_group),
                predicate::eq(group_names),
            )
            .returning(|_, _, _| Ok(()));

        self
    }
    pub fn expect_join_group_with(&mut self, name: &str, group_name: &str) -> &mut Self {
        let name = name.to_string();
        let group_name = group_name.to_string();

        self.expect_join_group()
            .with(predicate::eq(name), predicate::eq(group_name))
            .returning(|_, _| Ok(()));

        self
    }
}

#[cfg(target_os = "macos")]
pub fn create_for_target(
    os: Arc<dyn Os + Sync + Send + 'static>,
) -> Arc<dyn Credentials + Sync + Send> {
    Arc::new(MacOSCredentials::new(Arc::clone(&os)))
}

#[cfg(target_os = "linux")]
pub fn create_for_target(
    os: Arc<dyn Os + Sync + Send + 'static>,
) -> Arc<dyn Credentials + Sync + Send> {
    Arc::new(LinuxCredentials::new(Arc::clone(&os)))
}
