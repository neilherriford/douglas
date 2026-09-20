use crate::bootstrap::{self, rollback, upgrade::Direction};
use crate::cli::Presentation;
use crate::commands::{CommandContext, print_error};
use crate::verify::{BinaryVerifier, DouglasBinaryVerifier};
use credentials::create_credentials;
use file_system::{
    FileReader, UnixFileCopier, UnixFileDeleter, UnixFileReader, UnixFolder, UnixPermissions,
};
use log::{Level, ScopeKind, Span};
use os::{Os, Unix};
use std::process::ExitCode;
use std::sync::Arc;

pub(crate) async fn rollback(
    plan_only: bool,
    to: Option<String>,
    presentation: Presentation,
) -> ExitCode {
    let Some(CommandContext {
        douglas_folders,
        reporter,
    }) = CommandContext::for_presentation(presentation)
    else {
        return ExitCode::from(1);
    };

    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials: Arc<dyn credentials::Credentials> =
        Arc::from(create_credentials(Arc::clone(&os)));
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());

    let fail = |message: &str| {
        Span::new(
            Arc::clone(&reporter),
            "Rolling back douglas",
            ScopeKind::Task,
        )
        .message(Level::Warn, message);
        if let Some(style) = presentation.console_style() {
            print_error(style, message);
        }
        ExitCode::from(1)
    };

    if !credentials.is_root() {
        return fail("Must be root to proceed");
    }

    let verifier = DouglasBinaryVerifier::new(Arc::clone(&os), Arc::clone(&file_reader));
    let current = match verifier.get_internal_release() {
        Ok(release) => release.version,
        Err(err) => return fail(&format!("Could not determine the running version: {err}")),
    };

    let target =
        match rollback::choose(&douglas_folders, &UnixFolder::new(), current, to.as_deref()) {
            Ok(target) => target,
            Err(err) => return fail(&err.to_string()),
        };

    let deleter = UnixFileDeleter::new();
    let staged = match rollback::stage(
        &douglas_folders,
        &UnixFileCopier::new(),
        &UnixPermissions::new(),
        &deleter,
        target,
    ) {
        Ok(staged) => staged,
        Err(err) => return fail(&err.to_string()),
    };

    let succeeded = bootstrap::upgrade::perform(
        reporter.clone(),
        plan_only,
        false,
        Direction::Rollback,
        bootstrap::upgrade::Dependencies {
            credentials,
            os,
            douglas_folders,
            file_reader,
        },
        &staged,
        presentation,
    )
    .await;

    rollback::unstage(&deleter, &staged);

    if !succeeded {
        return fail("Rollback failed");
    }

    if plan_only {
        eprintln!("Rollback plan only.");
    }
    ExitCode::from(0)
}
