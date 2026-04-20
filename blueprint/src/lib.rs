use std::sync::Arc;

use log::Logger;
use thiserror::Error;

pub trait Command<TContext>: std::fmt::Display {
    type Error: std::fmt::Display;

    fn name(&self) -> &str;
    fn run(&mut self, logger: &dyn Logger, context: &mut TContext) -> Result<(), Self::Error>;
    fn rollback(
        &mut self,
        _logger: &dyn Logger,
        _context: &mut TContext,
    ) -> Result<(), Self::Error> {
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

pub enum ExecutionResult<TError> {
    Success,
    Failed {
        failed_at_step: usize,
        failed_at_step_name: String,
        perform_error: TError,
        rollback_errors: Vec<TError>,
    },
}

pub trait CommandExecutor<TContext> {
    type Error: std::fmt::Display;

    fn run(
        &mut self,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext, Error = Self::Error>>>,
    ) -> ExecutionResult<Self::Error>;

    fn rollback(&mut self, context: &mut TContext) -> Vec<Self::Error>;
}

pub struct JournalingExecutor<TContext, TError> {
    logger: Arc<dyn Logger>,
    journal: Vec<Box<dyn Command<TContext, Error = TError>>>,
}

impl<TContext, TError> JournalingExecutor<TContext, TError> {
    pub fn new(logger: Arc<dyn Logger>) -> Self {
        Self {
            logger,
            journal: vec![],
        }
    }
}

impl<TContext, TError> CommandExecutor<TContext> for JournalingExecutor<TContext, TError>
where
    TError: std::fmt::Display,
{
    type Error = TError;

    fn run(
        &mut self,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext, Error = Self::Error>>>,
    ) -> ExecutionResult<Self::Error> {
        for (step_index, mut command) in commands.into_iter().enumerate() {
            let step_name = command.name().to_string();

            if command.skip(context) {
                self.logger
                    .info(&format!("[{step_name}] already satisfied, skipping"));
                self.journal.push(command);
                continue;
            }

            self.logger.info(&format!("{command} performing"));

            if let Err(err) = command.run(&*self.logger, context) {
                self.logger.error(&format!("[{step_name}] failed: {err}"));
                self.logger.info("beginning rollback");

                let rollback_errors = self.rollback(context);
                return ExecutionResult::Failed {
                    failed_at_step: step_index,
                    failed_at_step_name: step_name,
                    perform_error: err,
                    rollback_errors,
                };
            }

            self.logger.info(&format!("{command} succeeded"));
            self.journal.push(command);
        }
        ExecutionResult::Success
    }

    fn rollback(&mut self, context: &mut TContext) -> Vec<Self::Error> {
        let mut errors = Vec::new();
        for mut cmd in self.journal.drain(..).rev() {
            self.logger
                .info(&format!("[Rolling back {}…] ", cmd.name()));

            if let Err(err) = cmd.rollback(&*self.logger, context) {
                self.logger
                    .error(&format!("[{}] rollback failed: {err}", cmd.name()));
                errors.push(err);
            }
        }
        errors
    }
}
