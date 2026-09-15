use crate::bootstrap;
use crate::cli::OutputStyle;
use crate::commands::print_error;
use crate::daemon::{build_cli_reporter, build_plain_reporter};
use ::config::DouglasFolders;
use credentials::create_credentials;
use file_system::{FileReader, UnixFileReader};
use log::Reporter;
use os::{Os, Unix};
use std::{process::ExitCode, sync::Arc};

pub(crate) async fn stop(plan_only: bool, output_style: Option<OutputStyle>) -> ExitCode {
    let douglas_folders = DouglasFolders::new();

    let reporter: Arc<dyn Reporter> = match output_style {
        Some(_) => build_plain_reporter(&douglas_folders, config::DOUGLAS_CLI_LOG_NAME),
        None => {
            if let Ok(reporter) = build_cli_reporter(&douglas_folders, config::DOUGLAS_CLI_LOG_NAME)
            {
                reporter
            } else {
                eprintln!("Failed to start TUI reporter");
                return ExitCode::from(1);
            }
        }
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
        if let Some(style) = output_style {
            print_error(style, "System stop failed");
        }
        return ExitCode::from(1);
    }

    if plan_only {
        eprintln!("Stop plan only.");
        return ExitCode::from(0);
    }

    ExitCode::from(0)
}
