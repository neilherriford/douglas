use crate::bootstrap::{
    self,
    compatibility::{self, CompatibilityError},
    journal,
};
use crate::cli::{OutputStyle, Presentation};
use crate::commands::{CommandContext, names, print_failure, print_success, report_failure};
use crate::util::join_display;
use ::config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    FileDeleter, FileReader, FileWriter, Folder, Inspect, Permissions, UnixFileDeleter,
    UnixFileReader, UnixFileWriter, UnixFolder, UnixInspect, UnixPermissions,
};
use identity::{Identity, LocalIdentity};
use log::Reporter;
use os::{EnvironmentVariableReader, Os, Unix, UnixEnvironmentVariableReader};
use release::ReleaseMetadata;
use std::{process::ExitCode, sync::Arc};

pub(crate) async fn start(plan_only: bool, presentation: Presentation) -> ExitCode {
    let Some(CommandContext {
        douglas_folders,
        reporter,
    }) = CommandContext::for_presentation(presentation)
    else {
        return ExitCode::from(1);
    };
    let output_style = presentation.console_style();

    let running = ReleaseMetadata::current();
    if let Err(err) =
        compatibility::ensure_compatible(&douglas_folders, &UnixFileReader::new(), &running)
    {
        report_compatibility_failure(&reporter, presentation, "Checking installed data", &err);
        return ExitCode::from(1);
    }

    let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
    warn_about_interrupted_upgrade(&reporter, presentation, &douglas_folders, os.as_ref());
    let credentials = Arc::from(create_credentials(Arc::clone(&os)));
    let permissions: Arc<dyn Permissions> = Arc::new(UnixPermissions::new());
    let environment_variable_reader: Arc<dyn EnvironmentVariableReader> =
        Arc::new(UnixEnvironmentVariableReader::new());

    let succeeded = bootstrap::system::perform(
        Arc::clone(&reporter),
        plan_only,
        bootstrap::system::Dependencies {
            credentials,
            permissions,
            environment_variable_reader,
            folder,
            os,
            douglas_folders: douglas_folders.clone(),
        },
    )
    .await;

    if !succeeded {
        print_failure(output_style, "System bootstrap failed");
        return ExitCode::from(1);
    }

    if plan_only {
        eprintln!(
            "System plan only: skipping seedling reconciliation, which requires bract to be running."
        );
        return ExitCode::from(0);
    }

    let bract_client: Arc<dyn bract_client::Client> = Arc::new(bract_client::UdsClient::new(
        Arc::clone(&reporter),
        &douglas_folders,
    ));

    let succeeded =
        bootstrap::core_seedlings::perform(Arc::clone(&reporter), Arc::clone(&bract_client)).await;

    if !succeeded {
        print_failure(output_style, "Seedling reconciliation failed");
        return ExitCode::from(1);
    }

    if !bootstrap_openbao(&reporter, output_style, &douglas_folders, &bract_client).await {
        return ExitCode::from(1);
    }

    log_deadwood_if_any(&reporter, bract_client.as_ref()).await;

    if let Err(err) = compatibility::record_install(
        &douglas_folders,
        &UnixFileWriter::new(),
        env!("CARGO_PKG_VERSION"),
        &running,
    ) {
        report_compatibility_failure(
            &reporter,
            presentation,
            "Recording the installed release",
            &err,
        );
        return ExitCode::from(1);
    }

    if let Some(style) = output_style {
        print_success(style, "Douglas started.");
    }

    ExitCode::from(0)
}

fn warn_about_interrupted_upgrade(
    reporter: &Arc<dyn Reporter>,
    presentation: Presentation,
    douglas_folders: &DouglasFolders,
    os: &dyn Os,
) {
    let message = match journal::load(douglas_folders, &UnixFileReader::new()) {
        Ok(Some(entry)) if journal::is_interrupted(os, &entry) => {
            journal::describe_interrupted(&entry)
        }
        Ok(_) => return,
        Err(err) => err.to_string(),
    };

    log::Span::new(
        Arc::clone(reporter),
        "Checking for an interrupted upgrade",
        log::ScopeKind::Task,
    )
    .message(log::Level::Warn, &message);

    if presentation == Presentation::Plain {
        eprintln!("{message}");
    }
}

fn report_compatibility_failure(
    reporter: &Arc<dyn Reporter>,
    presentation: Presentation,
    label: &str,
    err: &CompatibilityError,
) {
    let guard = log::Span::new(Arc::clone(reporter), label, log::ScopeKind::Task).start_guard();
    report_failure(guard.span(), presentation.console_style(), &err.to_string());
    guard.finish_with_outcome(log::Outcome::Failed);
}

async fn bootstrap_openbao(
    reporter: &Arc<dyn Reporter>,
    output_style: Option<OutputStyle>,
    douglas_folders: &DouglasFolders,
    bract_client: &Arc<dyn bract_client::Client>,
) -> bool {
    let inspect: Arc<dyn Inspect> = Arc::new(UnixInspect {});
    let openbao_client_factory: Arc<dyn openbao::ClientFactory> =
        Arc::new(openbao::SocketClientFactory::new(Arc::clone(reporter)));
    let openbao_file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader {});
    let openbao_file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter {});
    let openbao_file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter {});
    let mut identity = LocalIdentity::new(
        Arc::clone(&openbao_file_reader),
        Arc::clone(&openbao_file_writer),
    );

    if let Err(err) = identity.initialize() {
        print_failure(
            output_style,
            &format!("Failed to initialize identity: {err}"),
        );
        return false;
    }

    let succeeded = bootstrap::openbao::perform(
        Arc::clone(reporter),
        bootstrap::openbao::Dependencies {
            inspect,
            openbao_client_factory,
            bract_client: Arc::clone(bract_client),
            file_reader: openbao_file_reader,
            file_writer: openbao_file_writer,
            file_deleter: openbao_file_deleter,
            permissions: Arc::new(UnixPermissions::new()),
            identity: &mut identity,
            douglas_folders,
        },
    )
    .await;

    if !succeeded {
        print_failure(output_style, "OpenBao bootstrap failed");
        return false;
    }

    true
}

async fn log_deadwood_if_any(
    reporter: &Arc<dyn Reporter>,
    bract_client: &dyn bract_client::Client,
) {
    let guard = log::Span::new(
        Arc::clone(reporter),
        "Checking for deadwood",
        log::ScopeKind::Task,
    )
    .start_guard();

    match bract_client.find_deadwood().await {
        Ok(deadwood) if deadwood.is_empty() => guard.finish_with_outcome(log::Outcome::Ok),
        Ok(deadwood) => {
            let details = [
                ("container", names(&deadwood.containers)),
                ("network", names(&deadwood.networks)),
                ("route file", names(&deadwood.route_files)),
                ("resin repository", deadwood.resin_repositories.clone()),
                ("mount", names(&deadwood.mounts)),
                ("openbao secret", names(&deadwood.openbao_secrets)),
            ]
            .into_iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(label, names)| format!("{label}(s): {}", join_display(&names, ", ")))
            .collect::<Vec<_>>();
            let details = join_display(&details, "; ");

            guard.span().message(
                log::Level::Warn,
                &format!("Found deadwood (run `douglas seedling prune` to clean up): {details}"),
            );
            guard.finish_with_outcome(log::Outcome::Ok);
        }
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Could not check for deadwood: {err}"),
            );
            guard.finish_with_outcome(log::Outcome::Failed);
        }
    }
}
