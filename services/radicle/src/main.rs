mod bract_path_factory;
mod command_output_printer;
mod config;
mod constants;
mod douglas_logger_factory;
mod file_logger;
mod init_command;
mod shutdown_command;
mod start_bract_command;
mod status_command;
mod verbose_printer;

use bract_path_factory::BractPathFactory;
use clap::{Parser, Subcommand, ValueEnum};
use config::{ConfigReader, LocalConfigRepository};
use credentials::create_for_target;
use file_system::{
    FileAppender, FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links,
    LocalFileAppender, LocalFileDeleter, LocalFileReader, LocalFileWriter, LocalFolder, LocalLinks,
    LocalPermissions, LocalUnixDomainSocket, Permissions, UnixDomainSocket,
};
use init_command::InitCommand;

use command_output_printer::{
    CommandOutputPrinter, JsonCommandOutputPrinter, PlainCommandOutputPrinter,
};
use log::{Logger, StdOutLogger};
use os::{Os, Unix};
use shutdown_command::ShutdownCommand;
use start_bract_command::{BractLogger, StartBractCommand};
use status_command::{DouglasStatus, StatusCommand};
use std::{fmt::Debug, path::Path, sync::Arc};
use verbose_printer::{PlainVerbosePrinter, SilentVerbosePrinter, VerbosePrinter};

#[derive(ValueEnum, Clone, Debug)]
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
    #[command(long_about = "Initializes the bract subsystem")]
    Init {
        #[arg(long, help = "Required: Permitted CLI user")]
        service_user: String,

        #[arg(long, help = "Required: Permitted CLI group")]
        service_group: String,

        #[arg(
            long,
            help = "Required: Location where container mounts will be stored"
        )]
        mount_root_path: String,

        #[arg(long, help = "Required: Location where douglas will write logs")]
        log_path: String,

        #[arg(long, help = "Required: The Docker socket location")]
        docker_socket_path: String,

        #[arg(
            long,
            default_value_t = true,
            help = "Required: When true, bract is run as daemon.  Defaults to true"
        )]
        daemonize: bool,
    },

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

fn main() {
    let stdout_log: Arc<dyn Logger + Send + Sync> = Arc::new(StdOutLogger::new());
    let os: Arc<dyn Os + Sync + Send> = Arc::new(Unix::new());
    let credentials = create_for_target(os.clone());
    let folder: Arc<dyn Folder + Send + Sync> = Arc::new(LocalFolder::new());
    let permissions: Arc<dyn Permissions + Send + Sync> = Arc::new(LocalPermissions::new());
    let file_reader: Arc<dyn FileReader + Send + Sync> = Arc::new(LocalFileReader::new());
    let file_writer: Arc<dyn FileWriter + Send + Sync> = Arc::new(LocalFileWriter::new());
    let file_deleter: Arc<dyn FileDeleter + Send + Sync> = Arc::new(LocalFileDeleter::new());
    let file_appender: Arc<dyn FileAppender + Send + Sync> = Arc::new(LocalFileAppender::new());
    let links: Arc<dyn Links + Sync + Send> = Arc::new(LocalLinks::new());
    let config_repository = Arc::new(LocalConfigRepository::new(
        folder.clone(),
        permissions.clone(),
        file_reader.clone(),
        file_writer.clone(),
    ));
    let bract_path_factory = Arc::new(BractPathFactory::new(folder.clone()));
    let unix_domain_socket: Arc<dyn UnixDomainSocket + 'static> =
        Arc::new(LocalUnixDomainSocket::new());

    let cli = Cli::parse();

    let verbose_printer: Arc<dyn VerbosePrinter> = match cli.verbose {
        Switch::Enabled => Arc::new(PlainVerbosePrinter::new()),
        Switch::Disabled => Arc::new(SilentVerbosePrinter::new()),
    };

    match cli.command {
        Commands::Init {
            service_user,
            service_group,
            mount_root_path,
            log_path,
            docker_socket_path,
            daemonize,
        } => {
            let init_result = InitCommand::new(
                &service_user,
                &service_group,
                Path::new(&mount_root_path),
                Path::new(&log_path),
                Path::new(&docker_socket_path),
                credentials.clone(),
                folder.clone(),
                permissions.clone(),
                config_repository.clone(),
                Arc::clone(&verbose_printer),
            )
            .perform();

            if init_result.is_err() {
                create_simple_printer(cli.output_style).print("init", &init_result);
                return;
            }

            let log = if daemonize {
                BractLogger::WriteToFile
            } else {
                BractLogger::Use(Arc::clone(&stdout_log))
            };

            let start_bract_result = StartBractCommand::new(
                Arc::clone(&credentials),
                Arc::clone(&folder),
                config_repository.clone(),
                Arc::clone(&file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&file_appender),
                Arc::clone(&links),
                Arc::clone(&os),
                Arc::clone(&permissions),
                Arc::clone(&unix_domain_socket),
                Arc::clone(&bract_path_factory),
                log,
                Arc::clone(&verbose_printer),
            )
            .perform(daemonize);

            create_simple_printer(cli.output_style).print("init", &start_bract_result);
        }
        Commands::Start { daemonize } => {
            let log = if daemonize {
                BractLogger::WriteToFile
            } else {
                BractLogger::Use(Arc::clone(&stdout_log))
            };

            let start_bract_result = StartBractCommand::new(
                Arc::clone(&credentials),
                Arc::clone(&folder),
                config_repository.clone(),
                Arc::clone(&file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&file_appender),
                Arc::clone(&links),
                Arc::clone(&os),
                Arc::clone(&permissions),
                Arc::clone(&unix_domain_socket),
                Arc::clone(&bract_path_factory),
                log,
                Arc::clone(&verbose_printer),
            )
            .perform(daemonize);

            create_simple_printer(cli.output_style).print("start", &start_bract_result);
        }
        Commands::Status => {
            let result = StatusCommand::new(
                Arc::clone(&file_reader),
                Arc::clone(&stdout_log),
                Arc::clone(&bract_path_factory),
            )
            .perform();

            make_status_printer(cli.output_style).print("status", &result);
        }
        Commands::Shutdown => {
            let result = ShutdownCommand::new(
                Arc::clone(&bract_path_factory),
                Arc::clone(&config_repository) as Arc<dyn ConfigReader + Send + Sync>,
                Arc::clone(&file_appender),
                Arc::clone(&file_reader),
                Arc::clone(&permissions),
            )
            .perform();
            create_simple_printer(cli.output_style).print("shutdown", &result);
        }
    }
}

fn make_status_printer(
    style: OutputStyle,
) -> Box<dyn CommandOutputPrinter<DouglasStatus, FileSystemError>> {
    match style {
        OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
        OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
    }
}

// fn make_init_printer(style: OutputStyle) -> Box<dyn CommandOutputPrinter<(), InitCommandError>> {
//     match style {
//         OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
//         OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
//     }
// }

// fn make_start_bract_printer(
//     style: OutputStyle,
// ) -> Box<dyn CommandOutputPrinter<(), StartBractCommandError>> {
//     match style {
//         OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
//         OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
//     }
// }

fn create_simple_printer<T>(style: OutputStyle) -> Box<dyn CommandOutputPrinter<(), T>>
where
    T: std::fmt::Display,
{
    match style {
        OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
        OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
    }
}
