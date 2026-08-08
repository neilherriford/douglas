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

pub fn resolve_plan<TContext, E>(
    span: &Span,
    plan: Result<Vec<Box<dyn Command<TContext>>>, E>,
) -> Result<Vec<Box<dyn Command<TContext>>>, E> {
    if let Ok(plan) = &plan {
        span.plan_hint(plan.iter().map(std::string::ToString::to_string).collect());
    }
    plan
}

pub async fn execute_plan<TContext: Send, E>(
    span: &Span,
    plan: Vec<Box<dyn Command<TContext>>>,
    context: &mut TContext,
    on_failed: impl FnOnce(String) -> E,
) -> Result<(), E> {
    let mut executor = JournalingExecutor::new();
    match executor
        .run(
            &span.create_child("Executing plan", ScopeKind::Phase),
            context,
            plan,
        )
        .await
    {
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
            Err(on_failed(format!(
                "step {failed_at_step} ({failed_at_step_name}): {perform_error}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use log::Event;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn root_span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    struct SucceedingCommand;

    impl std::fmt::Display for SucceedingCommand {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Succeeding")
        }
    }

    #[async_trait]
    impl crate::Command<()> for SucceedingCommand {
        fn name(&self) -> String {
            "Succeeding".to_string()
        }

        async fn run(
            &mut self,
            _span: &Span,
            _context: &mut (),
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    struct FailingCommand;

    impl std::fmt::Display for FailingCommand {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Failing")
        }
    }

    #[async_trait]
    impl crate::Command<()> for FailingCommand {
        fn name(&self) -> String {
            "Failing".to_string()
        }

        async fn run(
            &mut self,
            _span: &Span,
            _context: &mut (),
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Err("boom".into())
        }
    }

    #[tokio::test]
    async fn test_execute_plan_should_succeed_when_every_step_succeeds() {
        let span = root_span();
        let mut context = ();
        let plan: Vec<Box<dyn crate::Command<()>>> = vec![Box::new(SucceedingCommand)];

        let result = execute_plan(&span, plan, &mut context, |reason| reason).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_plan_should_pass_the_failure_reason_to_on_failed() {
        let span = root_span();
        let mut context = ();
        let plan: Vec<Box<dyn crate::Command<()>>> =
            vec![Box::new(SucceedingCommand), Box::new(FailingCommand)];

        let result = execute_plan(&span, plan, &mut context, |reason| reason).await;

        let reason = result.expect_err("should fail");
        assert!(reason.contains("step 1"));
        assert!(reason.contains("Failing"));
        assert!(reason.contains("boom"));
    }
}
