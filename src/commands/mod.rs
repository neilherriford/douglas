pub(crate) mod kick;
pub(crate) mod prune;
pub(crate) mod seedling;
pub(crate) mod start;
pub(crate) mod status;
pub(crate) mod stop;

use crate::cli::OutputStyle;
use crate::daemon::build_plain_reporter;
use ::config::DouglasFolders;
use crossterm::style::Stylize;
use log::Span;

pub(crate) fn seedling_command_context(label: &str) -> (DouglasFolders, log::ScopeGuard) {
    let douglas_folders = DouglasFolders::new();
    let reporter = build_plain_reporter(&douglas_folders, config::DOUGLAS_CLI_LOG_NAME);
    let guard = Span::new(reporter, label, log::ScopeKind::Task).start_guard();
    (douglas_folders, guard)
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

pub(crate) fn parse_seedling_name(
    guard: &log::ScopeGuard,
    output_style: OutputStyle,
    name: &str,
) -> Option<seedbank_types::Name> {
    if let Ok(name) = name.parse() {
        Some(name)
    } else {
        guard
            .span()
            .message(log::Level::Warn, "Invalid seedling name");
        print_error(output_style, "Invalid seedling name");
        None
    }
}

pub(crate) fn names(items: &[seedbank_types::Name]) -> Vec<String> {
    items.iter().map(std::string::ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
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
}
