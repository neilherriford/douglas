use crate::bootstrap::{self, upgrade::Direction};
use crate::cli::Presentation;
use crate::commands::{CommandContext, print_error};
use credentials::create_credentials;
use file_system::{FileReader, UnixFileReader};
use os::{Os, Unix};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

pub(crate) async fn upgrade(
    plan_only: bool,
    allow_one_way: bool,
    presentation: Presentation,
    path: PathBuf,
) -> ExitCode {
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

    let succeeded = bootstrap::upgrade::perform(
        reporter,
        plan_only,
        allow_one_way,
        Direction::Upgrade,
        bootstrap::upgrade::Dependencies {
            credentials,
            os,
            douglas_folders,
            file_reader,
        },
        &path,
        presentation,
    )
    .await;

    if !succeeded {
        if let Some(style) = presentation.console_style() {
            print_error(style, "Upgrade failed");
        }
        return ExitCode::from(1);
    }

    if plan_only {
        eprintln!("Upgrade plan only.");
        return ExitCode::from(0);
    }

    ExitCode::from(0)
}
