use log::{Level, Span};
use thiserror::Error;

pub trait Command<TContext>: std::fmt::Display {
    type Error: std::fmt::Display;

    fn name(&self) -> String;
    fn run(&mut self, span: &Span, context: &mut TContext) -> Result<(), Self::Error>;
    fn rollback(&mut self, _span: &Span, _context: &mut TContext) -> Result<(), Self::Error> {
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
        span: &Span,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext, Error = Self::Error>>>,
    ) -> ExecutionResult<Self::Error>;

    fn rollback(&mut self, span: &Span, context: &mut TContext) -> Vec<Self::Error>;
}

pub struct JournalingExecutor<TContext, TError> {
    journal: Vec<Box<dyn Command<TContext, Error = TError>>>,
}

impl<TContext, TError> JournalingExecutor<TContext, TError> {
    pub fn new() -> Self {
        Self { journal: vec![] }
    }
}

impl<TContext, TError> Default for JournalingExecutor<TContext, TError> {
    fn default() -> Self {
        Self::new()
    }
}

impl<TContext, TError> CommandExecutor<TContext> for JournalingExecutor<TContext, TError>
where
    TError: std::fmt::Display,
{
    type Error = TError;

    fn run(
        &mut self,
        span: &Span,
        context: &mut TContext,
        commands: Vec<Box<dyn Command<TContext, Error = Self::Error>>>,
    ) -> ExecutionResult<Self::Error> {
        for (step_index, mut command) in commands.into_iter().enumerate() {
            let step_name = command.name().to_string();

            if command.skip(context) {
                span.message(
                    Level::Info,
                    &format!("[{step_name}] already satisfied, skipping"),
                );
                self.journal.push(command);
                continue;
            }

            span.message(log::Level::Info, &format!("{command} performing"));

            if let Err(err) = command.run(span, context) {
                span.message(log::Level::Warn, &format!("[{step_name}] failed: {err}"));
                let rollback_errors = self.rollback(span, context);
                return ExecutionResult::Failed {
                    failed_at_step: step_index,
                    failed_at_step_name: step_name,
                    perform_error: err,
                    rollback_errors,
                };
            }

            span.message(Level::Info, &format!("{command} succeeded"));

            self.journal.push(command);
        }
        ExecutionResult::Success
    }

    fn rollback(&mut self, span: &Span, context: &mut TContext) -> Vec<Self::Error> {
        let mut errors = Vec::new();
        for mut cmd in self.journal.drain(..).rev() {
            span.message(Level::Info, &format!("[Rolling back {}…] ", cmd.name()));

            if let Err(err) = cmd.rollback(span, context) {
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
