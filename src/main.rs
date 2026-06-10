mod application_definition;
mod commands;
mod config;
mod core_applications;
mod douglas_flags_reader;
#[macro_use]
pub(crate) mod macros;
mod cli_reporter;
mod mount_file_template_expander;

use crate::cli_reporter::CliReporter;
use ::config::DouglasFolders;
use clap::{Parser, Subcommand, ValueEnum};
use credentials::create_credentials;
use daemonize::Daemonize;
use file_system::UnixFileReader;
use log::{BufferedFileReporter, Reporter, TeeReporter};
use os::{Os, Unix};
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
    #[command(long_about = "Starts the bract server")]
    Start {
        #[arg(
            long,
            default_value_t = false,
            help = "Optional: When true, bract is run as daemon.  Defaults to false"
        )]
        bract: bool,
        #[arg(long, help = "The file descriptor to stream boot information to.")]
        notify_fd: Option<i32>,
        #[arg(
            long,
            default_value_t = false,
            help = "Optional: When true, only displays the start plan"
        )]
        plan_only: bool,
    },
    #[command(long_about = "Start the resin server")]
    Resin,
    #[command(long_about = "Request Bract status")]
    Status,
    #[command(long_about = "Shutdown the bract server")]
    Shutdown,
}

impl Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::Start { bract, .. } => {
                if *bract {
                    f.write_str("start bract")
                } else {
                    f.write_str("start")
                }
            }
            Commands::Resin => f.write_str("status"),
            Commands::Status => f.write_str("status"),
            Commands::Shutdown => f.write_str("shutdown"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            bract: true,
            plan_only,
            notify_fd,
        } => start_bract(plan_only, notify_fd),
        Commands::Start { plan_only, .. } => run_with_tokio(start(plan_only)),
        Commands::Resin => run_with_tokio(resin()),
        Commands::Status => todo!(),
        Commands::Shutdown => todo!(),
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
fn start_bract(_plan_only: bool, reporting_fd: Option<i32>) -> ExitCode {
    match Daemonize::new().start() {
        Ok(()) => {
            // Child process — no tokio runtime exists yet, safe to create one.
            run_with_tokio(run_bract_server(reporting_fd))
        }
        Err(err) => {
            eprintln!("Failed to daemonize bract server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_bract_server(reporting_fd: Option<i32>) -> ExitCode {
    let server = match bract::Server::build(reporting_fd).await {
        Ok(server) => server,
        Err(err) => {
            eprintln!("Failed to bootstrap bract: {err}");
            return ExitCode::from(1);
        }
    };
    match server.start() {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("Failed to start bract: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn start(plan_only: bool) -> ExitCode {
    let Ok(cli_reporter) = CliReporter::start() else {
        eprintln!("Failed to start TUI reporter");
        return ExitCode::from(1);
    };

    let douglas_folders = DouglasFolders::new();
    let file_reader = Arc::new(UnixFileReader::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials = Arc::from(create_credentials(Arc::clone(&os)));

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
        file_reader,
        os,
        douglas_folders,
    )
    .await;

    ExitCode::from(0)
}

async fn resin() -> ExitCode {
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
