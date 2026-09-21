pub(crate) mod kick;
pub(crate) mod prune;
pub(crate) mod rollback;
pub(crate) mod seedling;
pub(crate) mod start;
pub(crate) mod status;
pub(crate) mod stop;
pub(crate) mod upgrade;
pub(crate) mod verify;

use crate::cli::{OutputStyle, Presentation};
use crate::daemon::{build_cli_reporter, build_plain_reporter};
use ::config::DouglasFolders;
use crossterm::style::Stylize;
use log::{Reporter, ScopeGuard, Span};
use std::sync::Arc;

pub(crate) fn survive_hangup() -> Option<tokio::signal::unix::Signal> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok()
}

pub(crate) struct CommandContext {
    pub(crate) douglas_folders: DouglasFolders,
    pub(crate) reporter: Arc<dyn Reporter>,
}

impl CommandContext {
    pub(crate) fn plain() -> Self {
        let douglas_folders = DouglasFolders::new();
        let reporter = build_plain_reporter(&douglas_folders, config::DOUGLAS_CLI_LOG_NAME);
        Self {
            douglas_folders,
            reporter,
        }
    }

    pub(crate) fn for_presentation(presentation: Presentation) -> Option<Self> {
        if presentation != Presentation::Interactive {
            return Some(Self::plain());
        }

        let douglas_folders = DouglasFolders::new();
        if let Ok(reporter) = build_cli_reporter(&douglas_folders, config::DOUGLAS_CLI_LOG_NAME) {
            return Some(Self {
                douglas_folders,
                reporter,
            });
        }

        eprintln!("Failed to start TUI reporter");
        None
    }

    pub(crate) fn task(&self, label: &str) -> ScopeGuard {
        Span::new(Arc::clone(&self.reporter), label, log::ScopeKind::Task).start_guard()
    }
}

#[derive(serde::Serialize)]
struct JsonSuccessResponse<'a> {
    success: bool,
    message: &'a str,
}

pub(crate) fn print_success(output_style: OutputStyle, message: &str) {
    match output_style {
        OutputStyle::Plain => println!("{message}"),
        OutputStyle::Json => {
            let response = JsonSuccessResponse {
                success: true,
                message,
            };
            if let Ok(json) = serde_json::to_string(&response) {
                println!("{json}");
            }
        }
    }
}

#[derive(serde::Serialize)]
struct JsonStatusResponse<'a> {
    status: Option<&'a bract_types::SeedlingStatus>,
    success: bool,
    error_message: Option<String>,
}

pub(crate) fn print_status_json(
    status: &bract_types::SeedlingStatus,
) -> Result<(), serde_json::Error> {
    let response = JsonStatusResponse {
        status: Some(status),
        success: true,
        error_message: None,
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub(crate) fn print_error(output_style: OutputStyle, message: &str) {
    match output_style {
        OutputStyle::Plain => eprintln!("{}", message.red()),
        OutputStyle::Json => {
            let response = JsonStatusResponse {
                status: None,
                success: false,
                error_message: Some(message.to_string()),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                println!("{json}");
            }
        }
    }
}

pub(crate) fn print_failure(output_style: Option<OutputStyle>, message: &str) {
    if let Some(output_style) = output_style {
        print_error(output_style, message);
    }
}

pub(crate) fn report_failure(span: &Span, output_style: Option<OutputStyle>, message: &str) {
    span.message(log::Level::Warn, message);
    print_failure(output_style, message);
}

pub(crate) fn parse_seedling_name(
    guard: &log::ScopeGuard,
    output_style: OutputStyle,
    name: &str,
) -> Option<seedbank_types::Name> {
    if let Ok(name) = name.parse() {
        Some(name)
    } else {
        report_failure(guard.span(), Some(output_style), "Invalid seedling name");
        None
    }
}

pub(crate) fn names(items: &[seedbank_types::Name]) -> Vec<String> {
    items.iter().map(std::string::ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
    struct CapturingReporter {
        messages: std::sync::Mutex<Vec<(log::Level, String)>>,
    }

    impl log::Reporter for CapturingReporter {
        fn emit(&self, event: log::Event) {
            if let log::EventKind::Message { level, text } = event.kind {
                let Ok(mut messages) = self.messages.lock() else {
                    panic!("messages mutex poisoned");
                };
                messages.push((level, text));
            }
        }
    }

    #[test]
    fn test_report_failure_should_log_the_message_as_a_warning() {
        let reporter = Arc::new(CapturingReporter {
            messages: std::sync::Mutex::new(Vec::new()),
        });
        let span = Span::new(
            Arc::clone(&reporter) as Arc<dyn log::Reporter>,
            "test",
            log::ScopeKind::Task,
        );

        report_failure(&span, None, "it went wrong");

        let Ok(messages) = reporter.messages.lock() else {
            panic!("messages mutex poisoned");
        };
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].0, log::Level::Warn));
        assert_eq!(messages[0].1, "it went wrong");
    }

    use super::*;
    use std::sync::Arc;

    struct NullReporter;

    impl log::Reporter for NullReporter {
        fn emit(&self, _event: log::Event) {}
    }

    fn test_guard() -> log::ScopeGuard {
        Span::new(Arc::new(NullReporter), "test", log::ScopeKind::Task).start_guard()
    }

    fn seedling_name(value: &str) -> seedbank_types::Name {
        let Ok(name) = value.parse() else {
            panic!("'{value}' should be a valid seedling name");
        };
        name
    }

    #[test]
    fn test_parse_seedling_name_should_accept_a_valid_name() {
        let guard = test_guard();

        let result = parse_seedling_name(&guard, OutputStyle::Plain, "hello-world");

        assert_eq!(result, Some(seedling_name("hello-world")));
    }

    #[test]
    fn test_parse_seedling_name_should_reject_an_invalid_name() {
        let guard = test_guard();

        let result = parse_seedling_name(&guard, OutputStyle::Plain, "Not Valid!");

        assert_eq!(result, None);
    }

    #[test]
    fn test_names_should_stringify_every_seedling_name() {
        let items = vec![seedling_name("hello-world"), seedling_name("second-app")];

        let result = names(&items);

        assert_eq!(
            result,
            vec!["hello-world".to_string(), "second-app".to_string()]
        );
    }

    #[test]
    fn test_names_should_be_empty_for_an_empty_slice() {
        assert_eq!(names(&[]), Vec::<String>::new());
    }

    #[tokio::test]
    async fn test_survive_hangup_should_install_a_handler_so_a_hangup_does_not_end_the_process() {
        assert!(survive_hangup().is_some());
    }
}
