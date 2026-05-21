mod application_definition;
mod application_installer;
mod commands;
mod config;
mod core_applications;
mod douglas_flags_reader;
#[macro_use]
pub(crate) mod macros;
mod cli_reporter;
mod mount_file_template_expander;

use ::config::DouglasFolders;
use clap::{Parser, Subcommand, ValueEnum};
use credentials::create_credentials;
use file_system::{LocalFileReader, LocalFolder, LocalPermissions};
use log::{BufferedFileReporter, TeeReporter};
use os::{Os, Unix, UnixEnvironmentVariableReader};
use std::{
    fmt::{Debug, Display},
    sync::Arc,
};

use crate::cli_reporter::CliReporter;

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
            Commands::Status => f.write_str("status"),
            Commands::Shutdown => f.write_str("shutdown"),
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            bract,
            notify_fd,
            plan_only,
        } => {
            if bract {
                todo!()
            } else {
                match tokio::runtime::Runtime::new() {
                    Ok(rt) => {
                        rt.block_on(async {
                            let Ok(cli_reporter) = CliReporter::start() else {
                                panic!()
                            };
                            let config = DouglasFolders::new();
                            let file_reader = Arc::new(LocalFileReader::new());
                            let folder = Arc::new(LocalFolder::new());
                            let permissions = Arc::new(LocalPermissions::new());
                            let reporter = Arc::new(TeeReporter::new(vec![
                                Box::new(BufferedFileReporter::new(config.log_file("douglas-cli"))),
                                Box::new(cli_reporter),
                            ]));
                            let os: Arc<dyn Os> = Arc::new(Unix::new());
                            let credentials = Arc::from(create_credentials(Arc::clone(&os)));
                            let environment_variable_reader =
                                Arc::new(UnixEnvironmentVariableReader::new());

                            commands::cli_start(
                                reporter,
                                plan_only,
                                credentials,
                                folder,
                                file_reader,
                                permissions,
                                os,
                                environment_variable_reader,
                                config,
                            )
                            .await;
                        });
                    }
                    Err(err) => eprintln!("Runtime error: {err}"),
                }
            }
        }
        Commands::Status => {
            todo!()
        }
        Commands::Shutdown => {
            todo!()
        }
    }
}
