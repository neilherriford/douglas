mod application_definition;
mod application_installer;
mod commands;
mod core_applications;
mod deferred_file_logger;
mod douglas_flags_reader;
mod file_logger;
#[macro_use]
pub(crate) mod macros;
mod mount_file_template_expander;
mod shutdown_command;
mod start_bract_daemon_command;
mod start_command;
mod status_command;
mod tee_logger;

use clap::{Parser, Subcommand, ValueEnum};
use douglas_flags_reader::{
    DouglasFlag, DouglasFlagsReader, EnvironmentVariableDouglasFlagsReader,
};
use os::{EnvironmentVariableReader, UnixEnvironmentVariableReader};
use start_bract_daemon_command::StartBractDaemonCommand;
use std::{
    fmt::{Debug, Display},
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
        help = "When enabled, intermidiate messagse are displayed, hidden otherwise."
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
            help = "Required: When true, bract is run as daemon.  Defaults to false"
        )]
        daemonize: bool,
    },
    #[command(long_about = "Request Bract status")]
    Status,
    #[command(long_about = "Shutdown the bract server")]
    Shutdown,
}

impl Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::Start { daemonize } => f.write_str("start"),
            Commands::Status => f.write_str("status"),
            Commands::Shutdown => f.write_str("shutdown"),
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { daemonize } => {
            let douglas_flags_reader = EnvironmentVariableDouglasFlagsReader::new(Arc::new(
                UnixEnvironmentVariableReader::new(),
            )
                as Arc<dyn EnvironmentVariableReader>);

            if douglas_flags_reader
                .read()
                .contains(&DouglasFlag::BractOnly)
            {
                StartBractDaemonCommand::new().run();
            } else {
                match tokio::runtime::Runtime::new() {
                    Ok(rt) => {
                        rt.block_on(async {
                            if start_command::StartCommand::new().perform().await {
                                println!("Done-ish!");
                            } else {
                                eprintln!("Failed to start, exiting…");
                            }
                        });
                    }
                    Err(err) => eprintln!("Runtime error: {err}"),
                }
            }
        }
        Commands::Status => {
            if !status_command::StatusCommand::new().perform() {
                eprintln!("Failed to determine status, exiting…");
            }
        }
        Commands::Shutdown => {
            if !shutdown_command::ShutdownCommand::new().perform() {
                eprintln!("Failed to shutdown, exiting…");
            }
        }
    }
}
