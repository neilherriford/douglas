mod application_definition;
mod commands;
mod config;
mod core_applications;
#[macro_use]
pub(crate) mod macros;
mod cli_reporter;

use crate::cli_reporter::CliReporter;
use ::config::DouglasFolders;
use clap::{Parser, Subcommand, ValueEnum};
use credentials::{
    create_credentials,
    well_known::{DOUGLAS_RESIN_GROUP, DOUGLAS_RESIN_USER},
};
use daemonize::Daemonize;
use file_system::{FileReader, Folder, Permissions, UnixFileReader, UnixFolder, UnixPermissions};
use log::{BufferedFileReporter, Reporter, TeeReporter};
use os::{EnvironmentVariableReader, Os, Unix, UnixEnvironmentVariableReader};
use std::{
    fmt::{Debug, Display},
    process::ExitCode,
    sync::Arc,
};

#[derive(ValueEnum, Clone, Debug, Copy)]
enum OutputStyle {
    Plain,
    Json,
}

#[derive(ValueEnum, Clone, Debug)]
enum Switch {
    Enabled,
    Disabled,
}

#[derive(Parser, Debug)]
#[command(name = "bract")]
#[command(about = "Initialize and run the secure core of Douglas")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = OutputStyle::Plain,
        help = "Set the output mode"
    )]
    output_style: OutputStyle,
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = Switch::Enabled,
        help = "When enabled, intermediate messages are displayed, hidden otherwise."
    )]
    verbose: Switch,
}

#[derive(Subcommand, Debug)]
#[command(name = "douglas", about = "My awesome CLI", version = "1.0")]
enum Commands {
    #[command(about = "Start Douglas")]
    Start {
        #[arg(long, default_value_t = false, help = "Only display the start plan")]
        plan_only: bool,
    },
    #[command(about = "Stop Douglas")]
    Stop,
    #[command(hide = true)]
    Service {
        #[command(subcommand)]
        service: ServiceCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceCommand {
    Bract {
        #[arg(long, help = "File descriptor to stream boot information to")]
        notify_fd: i32,
    },
    Resin {
        #[arg(long, default_value_t = false, help = "Debug mode")]
        dbg: bool,
    },
}

impl Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::Start { .. } => f.write_str("start"),
            Commands::Stop => f.write_str("stop"),
            Commands::Service {
                service: ServiceCommand::Bract { .. },
            } => f.write_str("service bract"),
            Commands::Service {
                service: ServiceCommand::Resin { .. },
            } => f.write_str("service resin"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { plan_only } => run_with_tokio(start(plan_only)),
        Commands::Stop => todo!(),
        Commands::Service {
            service: ServiceCommand::Bract { notify_fd },
        } => start_bract(notify_fd),
        Commands::Service {
            service: ServiceCommand::Resin { dbg },
        } => {
            if dbg {
                if let Err(err) = drop_to_resin_user() {
                    eprintln!("Failed to drop to resin user: {err}");
                    return ExitCode::from(1);
                }
                run_with_tokio(resin_debug_mode())
            } else {
                start_resin()
            }
        }
    }
}

fn run_with_tokio(fut: impl std::future::Future<Output = ExitCode>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(fut),
        Err(err) => {
            eprintln!("Failed to start async runtime: {err}");
            ExitCode::from(1)
        }
    }
}

/// Daemonise bract and start the server.
///
/// This is intentionally a plain synchronous function. `fork()` (called
/// inside `Daemonize::start`) must happen before any tokio runtime exists —
/// forking inside a running runtime leaves the child with broken worker-thread
/// state and a deadlocked scheduler.
fn start_bract(reporting_fd: i32) -> ExitCode {
    match Daemonize::new().start() {
        Ok(()) => run_with_tokio(run_bract_server(reporting_fd)),
        Err(err) => {
            eprintln!("Failed to daemonize bract server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_bract_server(reporting_fd: i32) -> ExitCode {
    let bract = match bract::Bract::build(reporting_fd).await {
        Ok(bract) => bract,
        Err(err) => {
            eprintln!("Failed to start bract: {err:?}");
            return ExitCode::from(1);
        }
    };

    if let Err(err) = bract.start().await {
        eprintln!("Failed to start bract: {err:?}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}

fn start_resin() -> ExitCode {
    match Daemonize::new()
        .user(DOUGLAS_RESIN_USER)
        .group(DOUGLAS_RESIN_GROUP)
        .start()
    {
        Ok(()) => run_with_tokio(run_resin_server()),
        Err(err) => {
            eprintln!("Failed to daemonize bract server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_resin_server() -> ExitCode {
    let server = resin::Server::default();
    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(_) => ExitCode::from(1),
    }
}

async fn start(plan_only: bool) -> ExitCode {
    let Ok(cli_reporter) = CliReporter::start() else {
        eprintln!("Failed to start TUI reporter");
        return ExitCode::from(1);
    };

    let douglas_folders = DouglasFolders::new();
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials = Arc::from(create_credentials(Arc::clone(&os)));
    let permissions: Arc<dyn Permissions> = Arc::new(UnixPermissions::new());
    let environment_variable_reader: Arc<dyn EnvironmentVariableReader> =
        Arc::new(UnixEnvironmentVariableReader::new());

    let reporter: Arc<dyn Reporter> = Arc::new(TeeReporter::new(vec![
        Box::new(BufferedFileReporter::new(
            douglas_folders.log_file("douglas-cli"),
        )),
        Box::new(cli_reporter),
    ]));

    commands::cli_start(
        reporter,
        plan_only,
        credentials,
        permissions,
        environment_variable_reader,
        folder,
        file_reader,
        os,
        douglas_folders,
    )
    .await;

    ExitCode::from(0)
}

fn drop_to_resin_user() -> Result<(), Box<dyn std::error::Error>> {
    use nix::unistd::{User, setgid, setuid};

    let user = User::from_name(DOUGLAS_RESIN_USER)?
        .ok_or_else(|| format!("user '{DOUGLAS_RESIN_USER}' not found",))?;

    setgid(user.gid)?; // gid first — once you drop uid you can't change gid
    setuid(user.uid)?;
    Ok(())
}

async fn resin_debug_mode() -> ExitCode {
    let Ok(cli_reporter) = CliReporter::start() else {
        eprintln!("Failed to start TUI reporter");
        return ExitCode::from(1);
    };

    let server = resin::Server::new(Arc::new(cli_reporter));
    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(_) => ExitCode::from(1),
    }
}
