use crate::bootstrap;
use crate::cli::Presentation;
use crate::commands::{CommandContext, print_failure, print_success};
use credentials::create_credentials;
use file_system::{FileReader, UnixFileReader};
use os::{Os, Unix};
use std::{process::ExitCode, sync::Arc};

pub(crate) async fn stop(plan_only: bool, presentation: Presentation) -> ExitCode {
    let Some(CommandContext {
        douglas_folders,
        reporter,
    }) = CommandContext::for_presentation(presentation)
    else {
        return ExitCode::from(1);
    };

    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials = Arc::from(create_credentials(Arc::clone(&os)));
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());

    let succeeded = bootstrap::stop::perform(
        reporter,
        plan_only,
        bootstrap::stop::Dependencies {
            credentials,
            os,
            douglas_folders,
            file_reader,
        },
    )
    .await;

    if !succeeded {
        print_failure(presentation.console_style(), "System stop failed");
        return ExitCode::from(1);
    }

    if plan_only {
        eprintln!("Stop plan only.");
        return ExitCode::from(0);
    }

    if let Some(style) = presentation.console_style() {
        print_success(style, "Douglas stopped.");
    }

    ExitCode::from(0)
}
