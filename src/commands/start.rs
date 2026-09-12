use crate::bootstrap;
use crate::cli::OutputStyle;
use crate::commands::{names, print_error};
use crate::daemon::{build_cli_reporter, build_plain_reporter};
use ::config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    FileDeleter, FileReader, FileWriter, Folder, Inspect, Permissions, UnixFileDeleter,
    UnixFileReader, UnixFileWriter, UnixFolder, UnixInspect, UnixPermissions,
};
use identity::{Identity, LocalIdentity};
use log::Reporter;
use os::{EnvironmentVariableReader, Os, Unix, UnixEnvironmentVariableReader};
use std::{process::ExitCode, sync::Arc};

pub(crate) async fn start(plan_only: bool, output_style: Option<OutputStyle>) -> ExitCode {
    let douglas_folders = DouglasFolders::new();

    let reporter: Arc<dyn Reporter> = match output_style {
        Some(_) => build_plain_reporter(&douglas_folders, "douglas-cli"),
        None => {
            if let Ok(reporter) = build_cli_reporter(&douglas_folders, "douglas-cli") {
                reporter
            } else {
                eprintln!("Failed to start TUI reporter");
                return ExitCode::from(1);
            }
        }
    };

    let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
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
        if let Some(style) = output_style {
            print_error(style, "System bootstrap failed");
        }
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
        if let Some(style) = output_style {
            print_error(style, "Seedling reconciliation failed");
        }
        return ExitCode::from(1);
    }

    let inspect: Arc<dyn Inspect> = Arc::new(UnixInspect {});
    let openbao_client_factory: Arc<dyn openbao::ClientFactory> =
        Arc::new(openbao::SocketClientFactory::new(Arc::clone(&reporter)));
    let openbao_file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader {});
    let openbao_file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter {});
    let openbao_file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter {});
    let mut identity = LocalIdentity::new(
        Arc::clone(&openbao_file_reader),
        Arc::clone(&openbao_file_writer),
    );

    if let Err(err) = identity.initialize() {
        if let Some(style) = output_style {
            print_error(style, &format!("Failed to initialize identity: {err}"));
        }
        return ExitCode::from(1);
    }

    let succeeded = bootstrap::openbao::perform(
        Arc::clone(&reporter),
        bootstrap::openbao::Dependencies {
            inspect,
            openbao_client_factory,
            bract_client: Arc::clone(&bract_client),
            file_reader: openbao_file_reader,
            file_writer: openbao_file_writer,
            file_deleter: openbao_file_deleter,
            permissions: Arc::new(UnixPermissions::new()),
            identity: &mut identity,
            douglas_folders: &douglas_folders,
        },
    )
    .await;

    if !succeeded {
        if let Some(style) = output_style {
            print_error(style, "OpenBao bootstrap failed");
        }
        return ExitCode::from(1);
    }

    log_deadwood_if_any(&reporter, bract_client.as_ref()).await;

    if let Some(style) = output_style {
        print_start_result(style);
    }

    ExitCode::from(0)
}

fn print_start_result(output_style: OutputStyle) {
    match output_style {
        OutputStyle::Plain => println!("Douglas started."),
        OutputStyle::Json => {
            if let Ok(json) = serde_json::to_string(&serde_json::json!({ "success": true })) {
                println!("{json}");
            }
        }
    }
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
            .map(|(label, names)| format!("{label}(s): {}", names.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");

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
