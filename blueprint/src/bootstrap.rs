use crate::{Command, CommandExecutor, ExecutionResult, JournalingExecutor};
use log::{BufferedFileReporter, Level, PipeReporter, Reporter, ScopeKind, Span, TeeReporter};
use std::path::PathBuf;
use std::sync::Arc;

pub fn build_boot_reporter(log_path: PathBuf, reporting_fd: Option<i32>) -> Arc<dyn Reporter> {
    let mut sinks: Vec<Box<dyn Reporter>> = vec![Box::new(BufferedFileReporter::new(log_path))];
    if let Some(fd) = reporting_fd {
        sinks.push(Box::new(unsafe { PipeReporter::from_raw_fd(fd) }));
    }
    Arc::new(TeeReporter::new(sinks))
}

pub fn resolve_plan<TContext, E: std::fmt::Display>(
    span: &Span,
    plan: Result<Vec<Box<dyn Command<TContext>>>, E>,
) -> Result<Vec<Box<dyn Command<TContext>>>, E> {
    match plan {
        Ok(plan) => {
            span.plan_hint(plan.iter().map(std::string::ToString::to_string).collect());
            Ok(plan)
        }
        Err(err) => {
            span.message(Level::Warn, &err.to_string());
            Err(err)
        }
    }
}

pub fn execute_plan<TContext, E>(
    span: &Span,
    plan: Vec<Box<dyn Command<TContext>>>,
    context: &mut TContext,
    on_failed: impl FnOnce() -> E,
) -> Result<(), E> {
    let mut executor = JournalingExecutor::new();
    match executor.run(
        &span.create_child("Executing plan", ScopeKind::Phase),
        context,
        plan,
    ) {
        ExecutionResult::Success => Ok(()),
        ExecutionResult::Failed {
            failed_at_step,
            failed_at_step_name,
            perform_error,
            rollback_errors,
        } => {
            span.message(
                Level::Warn,
                &format!(
                    "Failed at step {failed_at_step}. {failed_at_step_name}: '{perform_error}'"
                ),
            );
            if !rollback_errors.is_empty() {
                span.message(
                    Level::Warn,
                    "Additionally, ran into these errors while rolling back",
                );
                for error in rollback_errors {
                    span.message(Level::Warn, &error.to_string());
                }
            }
            Err(on_failed())
        }
    }
}
