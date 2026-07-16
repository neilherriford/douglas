pub mod bootstrap;
pub mod commands;
pub mod listener;
pub mod service;

use async_trait::async_trait;
use credentials::Credentials;
use file_system::{Folder, Modes, Permissions};
use log::{Level, Span};
use std::path::PathBuf;
use thiserror::Error;

pub trait HasCredentials {
    fn credentials(&self) -> &dyn Credentials;
}

pub trait HasFolder {
    fn folder(&self) -> &dyn Folder;
}

pub trait HasPermissions {
    fn permissions(&self) -> &dyn Permissions;
}

pub struct StandardContext<'a> {
    pub credentials: &'a dyn Credentials,
    pub folder: &'a dyn Folder,
    pub permissions: &'a dyn Permissions,
}

impl<'a> StandardContext<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        folder: &'a dyn Folder,
        permissions: &'a dyn Permissions,
    ) -> Self {
        Self {
            credentials,
            folder,
            permissions,
        }
    }
}

impl<'a> HasCredentials for StandardContext<'a> {
    fn credentials(&self) -> &dyn Credentials {
        self.credentials
    }
}

impl<'a> HasFolder for StandardContext<'a> {
    fn folder(&self) -> &dyn Folder {
        self.folder
    }
}

impl<'a> HasPermissions for StandardContext<'a> {
    fn permissions(&self) -> &dyn Permissions {
        self.permissions
    }
}

#[async_trait(?Send)]
pub trait Command<TContext>: std::fmt::Display {
    fn name(&self) -> String;
    async fn run(
        &mut self,
        span: &Span,
        context: &mut TContext,
    ) -> Result<(), Box<dyn std::error::Error>>;
    async fn rollback(
        &mut self,
        _span: &Span,
        _context: &mut TContext,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn skip(&self, _context: &TContext) -> bool {
        false
    }
}

#[derive(Error, Debug)]
pub enum HistoryError {
    #[error("No current state")]
    NoCurrentState,
    #[error("Cannot rollback initial state")]
    CannotRollback,
}

pub struct History<T> {
    initial: T,
    rest: Vec<T>,
}

impl<T> History<T> {
    pub fn new(value: T) -> Self {
        Self {
            initial: value,
            rest: Vec::<T>::new(),
        }
    }

    pub fn current(&self) -> &T {
        self.rest.last().unwrap_or(&self.initial)
    }

    pub fn push(&mut self, value: T) {
        self.rest.push(value);
    }

    pub fn rollback(&mut self) -> Result<(), HistoryError> {
        if self.rest.len() > 1 {
            self.rest.pop();
            Ok(())
        } else {
            Err(HistoryError::CannotRollback)
        }
    }

    pub fn is_initial_state(&self) -> bool {
        self.rest.is_empty()
    }
}

pub enum ExecutionResult {
    Success,
    Failed {
        failed_at_step: usize,
        failed_at_step_name: String,
        perform_error: Box<dyn std::error::Error>,
        rollback_errors: Vec<Box<dyn std::error::Error>>,
    },
}

#[async_trait(?Send)]
pub trait CommandExecutor<TContext> {
    async fn run(
        &mut self,
        span: &Span,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext>>>,
    ) -> ExecutionResult;

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut TContext,
    ) -> Vec<Box<dyn std::error::Error>>;
}

pub struct JournalingExecutor<TContext> {
    journal: Vec<Box<dyn Command<TContext>>>,
}

impl<TContext> JournalingExecutor<TContext> {
    pub fn new() -> Self {
        Self { journal: vec![] }
    }
}

impl<TContext> Default for JournalingExecutor<TContext> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl<TContext> CommandExecutor<TContext> for JournalingExecutor<TContext> {
    async fn run(
        &mut self,
        span: &Span,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext>>>,
    ) -> ExecutionResult {
        for (step_index, mut command) in commands.into_iter().enumerate() {
            let step_name = command.name();

            if command.skip(context) {
                span.message(
                    Level::Info,
                    &format!("[{step_name}] already satisfied, skipping"),
                );
                self.journal.push(command);
                continue;
            }

            span.message(Level::Info, &format!("{command}…"));

            if let Err(err) = command.run(span, context).await {
                span.message(Level::Warn, &format!("[{step_name}] failed: {err:?}"));
                let rollback_errors = self.rollback(span, context).await;
                return ExecutionResult::Failed {
                    failed_at_step: step_index,
                    failed_at_step_name: step_name,
                    perform_error: err,
                    rollback_errors,
                };
            }

            self.journal.push(command);
        }
        ExecutionResult::Success
    }

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut TContext,
    ) -> Vec<Box<dyn std::error::Error>> {
        let mut errors = Vec::new();
        for mut cmd in self.journal.drain(..).rev() {
            span.message(Level::Info, &format!("[Rolling back {}…]", cmd.name()));
            if let Err(err) = cmd.rollback(span, context).await {
                span.message(
                    Level::Warn,
                    &format!("[{}] rollback failed: {err}", cmd.name()),
                );
                errors.push(err);
            }
        }
        errors
    }
}

pub enum CommandResult {
    Ok,
    Error,
    PlanOnly,
}

#[derive(Default, PartialEq, Eq)]
pub enum RunningStatus {
    Running,
    #[default]
    NotRunning,
    Unknown,
}

pub struct GroupMembershipRequirement {
    pub group_name: String,
    pub user_name: String,
}

impl GroupMembershipRequirement {
    pub fn new(group_name: &str, user_name: &str) -> Self {
        Self {
            group_name: group_name.to_string(),
            user_name: user_name.to_string(),
        }
    }
}

pub struct FolderOwnershipRequirement {
    pub path: PathBuf,
    pub owning_user_name: String,
    pub owning_group_name: String,
}

impl FolderOwnershipRequirement {
    pub fn new(path: PathBuf, owning_user_name: &str, owning_group_name: &str) -> Self {
        Self {
            path,
            owning_user_name: owning_user_name.to_string(),
            owning_group_name: owning_group_name.to_string(),
        }
    }
}

pub struct FolderModeRequirement {
    pub path: PathBuf,
    pub mode: Modes,
}
