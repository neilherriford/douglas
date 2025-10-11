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
use config::{Config, ConfigReader, ConfigWriter, LocalConfigRepository};
use credentials::{Credentials, create_for_target};
use file_system::{
    FileAppender, FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links,
    LocalFileAppender, LocalFileDeleter, LocalFileReader, LocalFileWriter, LocalFolder, LocalLinks,
    LocalPermissions, LocalUnixDomainSocket, Permissions, UnixDomainSocket,
};
use init_command::{InitCommand, InitCommandError};

use command_output_printer::{
    CommandOutputPrinter, JsonCommandOutputPrinter, PlainCommandOutputPrinter,
};
use log::{Logger, StdOutLogger};
use os::{Os, Unix};
use shutdown_command::{ShutdownCommand, ShutdownCommandError};
use start_bract_command::{BractLogger, StartBractCommand, StartBractCommandError};
use status_command::{DouglasStatus, StatusCommand};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::Arc,
};
use verbose_printer::{PlainVerbosePrinter, SilentVerbosePrinter, VerbosePrinter};

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
            let config = Config {
                docker_socket_path: PathBuf::from(docker_socket_path),
                log_path: PathBuf::from(log_path),
                mount_root_path: PathBuf::from(mount_root_path),
                operator_group: service_group,
                operator_user: service_user,
            };
            run_init(
                &*create_empty_command_output_printer(cli.output_style),
                &*create_empty_command_output_printer(cli.output_style),
                &verbose_printer,
                &config,
                daemonize,
            );
        }
        Commands::Start { daemonize } => {
            run_start(
                &*create_empty_command_output_printer(cli.output_style),
                &verbose_printer,
                daemonize,
            );
        }
        Commands::Status => {
            let printer: Box<dyn CommandOutputPrinter<DouglasStatus, FileSystemError>> =
                match cli.output_style {
                    OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
                    OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
                };
            run_status(&*printer);
        }
        Commands::Shutdown => {
            run_shutdown(&*create_empty_command_output_printer(cli.output_style));
        }
    }
}

fn run_shutdown(command_output_printer: &dyn CommandOutputPrinter<(), ShutdownCommandError>) {
    let folder: Arc<dyn Folder + Send + Sync> = Arc::new(LocalFolder::new());
    let permissions: Arc<dyn Permissions + Send + Sync> = Arc::new(LocalPermissions::new());
    let file_reader: Arc<dyn FileReader + Send + Sync> = Arc::new(LocalFileReader::new());
    let file_writer: Arc<dyn FileWriter + Send + Sync> = Arc::new(LocalFileWriter::new());
    let bract_path_factory = Arc::new(BractPathFactory::new(Arc::clone(&folder)));
    let config_repository: Arc<dyn ConfigReader> = Arc::new(LocalConfigRepository::new(
        Arc::clone(&folder),
        Arc::clone(&permissions),
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
    ));
    let file_appender: Arc<dyn FileAppender + Send + Sync> = Arc::new(LocalFileAppender::new());

    let result = ShutdownCommand::new(
        bract_path_factory,
        config_repository,
        file_appender,
        file_reader,
        permissions,
    )
    .perform();
    command_output_printer.print("shutdown", &result);
}

fn run_status(command_output_printer: &dyn CommandOutputPrinter<DouglasStatus, FileSystemError>) {
    let stdout_log: Arc<dyn Logger> = Arc::new(StdOutLogger::new());
    let folder: Arc<dyn Folder + Send + Sync> = Arc::new(LocalFolder::new());
    let permissions: Arc<dyn Permissions + Send + Sync> = Arc::new(LocalPermissions::new());
    let file_reader: Arc<dyn FileReader + Send + Sync> = Arc::new(LocalFileReader::new());
    let file_writer: Arc<dyn FileWriter + Send + Sync> = Arc::new(LocalFileWriter::new());
    let config_repository: Arc<dyn ConfigReader> = Arc::new(LocalConfigRepository::new(
        Arc::clone(&folder),
        Arc::clone(&permissions),
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
    ));
    let bract_path_factory = Arc::new(BractPathFactory::new(folder.clone()));

    let result = StatusCommand::new(
        file_reader,
        stdout_log,
        bract_path_factory,
        config_repository,
    )
    .perform();

    command_output_printer.print("status", &result);
}

fn run_start(
    command_output_printer: &dyn CommandOutputPrinter<(), StartBractCommandError>,
    verbose_printer: &Arc<dyn VerbosePrinter>,
    daemonize: bool,
) {
    let stdout_log: Arc<dyn Logger> = Arc::new(StdOutLogger::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials: Arc<dyn Credentials + Send + Sync> = create_for_target(os.clone());
    let folder: Arc<dyn Folder + Send + Sync> = Arc::new(LocalFolder::new());
    let permissions: Arc<dyn Permissions + Send + Sync> = Arc::new(LocalPermissions::new());
    let file_reader: Arc<dyn FileReader + Send + Sync> = Arc::new(LocalFileReader::new());
    let file_writer: Arc<dyn FileWriter + Send + Sync> = Arc::new(LocalFileWriter::new());
    let file_deleter: Arc<dyn FileDeleter + Send + Sync> = Arc::new(LocalFileDeleter::new());
    let file_appender: Arc<dyn FileAppender + Send + Sync> = Arc::new(LocalFileAppender::new());
    let links: Arc<dyn Links + Send + Sync> = Arc::new(LocalLinks::new());
    let config_repository: Arc<dyn ConfigReader> = Arc::new(LocalConfigRepository::new(
        Arc::clone(&folder),
        Arc::clone(&permissions),
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
    ));

    let bract_path_factory = Arc::new(BractPathFactory::new(Arc::clone(&folder)));
    let unix_domain_socket: Arc<dyn UnixDomainSocket> = Arc::new(LocalUnixDomainSocket::new());

    let log = if daemonize {
        BractLogger::WriteToFile
    } else {
        BractLogger::Use(Arc::clone(&stdout_log))
    };

    let start_bract_result = StartBractCommand::new(
        credentials,
        folder,
        config_repository,
        file_reader,
        file_writer,
        file_deleter,
        file_appender,
        links,
        os,
        permissions,
        unix_domain_socket,
        bract_path_factory,
        log,
        Arc::clone(verbose_printer),
    )
    .perform(daemonize);

    command_output_printer.print("start", &start_bract_result);
}

fn run_init(
    init_command_output_printer: &dyn CommandOutputPrinter<(), InitCommandError>,
    start_command_output_printer: &dyn CommandOutputPrinter<(), StartBractCommandError>,
    verbose_printer: &Arc<dyn VerbosePrinter>,
    config: &Config,
    daemonize: bool,
) {
    let stdout_log: Arc<dyn Logger> = Arc::new(StdOutLogger::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());
    let credentials: Arc<dyn Credentials + Send + Sync> = create_for_target(os.clone());
    let folder: Arc<dyn Folder + Send + Sync> = Arc::new(LocalFolder::new());
    let permissions: Arc<dyn Permissions + Send + Sync> = Arc::new(LocalPermissions::new());
    let file_reader: Arc<dyn FileReader + Send + Sync> = Arc::new(LocalFileReader::new());
    let file_writer: Arc<dyn FileWriter + Send + Sync> = Arc::new(LocalFileWriter::new());
    let file_deleter: Arc<dyn FileDeleter + Send + Sync> = Arc::new(LocalFileDeleter::new());
    let file_appender: Arc<dyn FileAppender + Send + Sync> = Arc::new(LocalFileAppender::new());
    let links: Arc<dyn Links + Send + Sync> = Arc::new(LocalLinks::new());
    let config_repository = Arc::new(LocalConfigRepository::new(
        Arc::clone(&folder),
        Arc::clone(&permissions),
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
    ));

    let bract_path_factory = Arc::new(BractPathFactory::new(folder.clone()));
    let unix_domain_socket: Arc<dyn UnixDomainSocket> = Arc::new(LocalUnixDomainSocket::new());

    let init_result = InitCommand::new(
        &config.operator_user,
        &config.operator_group,
        Path::new(&config.mount_root_path),
        Path::new(&config.log_path),
        Path::new(&config.docker_socket_path),
        Arc::clone(&credentials) as Arc<dyn Credentials>,
        Arc::clone(&folder) as Arc<dyn Folder>,
        Arc::clone(&permissions) as Arc<dyn Permissions>,
        Arc::clone(&config_repository) as Arc<dyn ConfigWriter>,
        Arc::clone(verbose_printer),
    )
    .perform();
    if init_result.is_err() {
        init_command_output_printer.print("init", &init_result);
        return;
    }

    let log = if daemonize {
        BractLogger::WriteToFile
    } else {
        BractLogger::Use(Arc::clone(&stdout_log))
    };
    let start_bract_result = StartBractCommand::new(
        credentials,
        folder,
        config_repository as Arc<dyn ConfigReader>,
        file_reader,
        file_writer,
        file_deleter,
        file_appender,
        links,
        os,
        permissions,
        unix_domain_socket,
        bract_path_factory,
        log,
        Arc::clone(verbose_printer),
    )
    .perform(daemonize);
    start_command_output_printer.print("init", &start_bract_result);
}

fn create_empty_command_output_printer<T>(
    style: OutputStyle,
) -> Box<dyn CommandOutputPrinter<(), T>>
where
    T: std::fmt::Display,
{
    match style {
        OutputStyle::Plain => Box::new(PlainCommandOutputPrinter::new()),
        OutputStyle::Json => Box::new(JsonCommandOutputPrinter::new()),
    }
}
