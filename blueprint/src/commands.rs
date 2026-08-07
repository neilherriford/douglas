use crate::{Command, HasCredentials, HasFolder, HasPermissions};
use async_trait::async_trait;
use file_system::Modes;
use log::{Outcome, ScopeKind, Span};
use std::path::PathBuf;

pub struct CreateUser {
    user_name: String,
    primary_group_name: String,
}

impl CreateUser {
    pub fn new(user_name: &str, primary_group_name: &str) -> Self {
        Self {
            user_name: user_name.to_string(),
            primary_group_name: primary_group_name.to_string(),
        }
    }
}
#[async_trait]
impl<C: HasCredentials + Send> Command<C> for CreateUser {
    fn name(&self) -> String {
        "Create user".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Created user '{}'!", self.user_name),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .credentials()
            .create_user(&self.user_name, &self.primary_group_name, vec![])?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

impl std::fmt::Display for CreateUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "Create user '{}' with primary group '{}'",
            self.user_name, self.primary_group_name
        ))
    }
}

pub struct CreateGroup {
    group_name: String,
}

impl CreateGroup {
    pub fn new(group_name: &str) -> Self {
        Self {
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for CreateGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create group '{}'", self.group_name)
    }
}

#[async_trait]
impl<C: HasCredentials + Send> Command<C> for CreateGroup {
    fn name(&self) -> String {
        "Create group".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Created group '{}'!", self.group_name),
                ScopeKind::Step,
            )
            .start_guard();
        context.credentials().create_group(&self.group_name)?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub struct AddUserToGroup {
    user_name: String,
    group_name: String,
}

impl AddUserToGroup {
    pub fn new(user_name: &str, group_name: &str) -> Self {
        Self {
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for AddUserToGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Add user '{}' to group '{}'",
            self.user_name, self.group_name
        )
    }
}

#[async_trait]
impl<C: HasCredentials + Send> Command<C> for AddUserToGroup {
    fn name(&self) -> String {
        "Add user to group".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Added user '{}' to group '{}'!",
                    self.user_name, self.group_name
                ),
                ScopeKind::Step,
            )
            .start_guard();
        context
            .credentials()
            .join_group(&self.user_name, &self.group_name)?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub struct CreateFolder {
    folder: PathBuf,
}

impl CreateFolder {
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }
}

impl std::fmt::Display for CreateFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create folder '{}'", self.folder.display())
    }
}

#[async_trait]
impl<C: HasFolder + Send> Command<C> for CreateFolder {
    fn name(&self) -> String {
        "Create folder".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Created folder '{}'!", self.folder.display()),
                ScopeKind::Step,
            )
            .start_guard();
        context.folder().create_recursively(&self.folder)?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub struct SetOwnership {
    path: PathBuf,
    user_name: String,
    group_name: String,
}

impl SetOwnership {
    pub fn new(path: PathBuf, user_name: &str, group_name: &str) -> Self {
        Self {
            path,
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
        }
    }
}

impl std::fmt::Display for SetOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Set ownership on '{}' to user '{}' group '{}'",
            self.path.display(),
            self.user_name,
            self.group_name,
        )
    }
}

#[async_trait]
impl<C: HasPermissions + Send> Command<C> for SetOwnership {
    fn name(&self) -> String {
        "Set ownership".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Set ownership on '{}' to user '{}' group '{}'!",
                    self.path.display(),
                    self.user_name,
                    self.group_name,
                ),
                ScopeKind::Step,
            )
            .start_guard();
        context.permissions().change_user_and_group_ownership(
            &self.path,
            &self.user_name,
            &self.group_name,
        )?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub struct SetMode {
    path: PathBuf,
    mode: Modes,
}

impl SetMode {
    pub fn new(path: PathBuf, mode: Modes) -> Self {
        Self { path, mode }
    }
}

impl std::fmt::Display for SetMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Set mode on '{}' to '{}'",
            self.path.display(),
            self.mode
        )
    }
}

#[async_trait]
impl<C: HasPermissions + Send> Command<C> for SetMode {
    fn name(&self) -> String {
        "Set mode".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Set mode on '{}' to '{}'!",
                    self.path.display(),
                    self.mode
                ),
                ScopeKind::Step,
            )
            .start_guard();
        context.permissions().change_mode(&self.path, &self.mode)?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}
