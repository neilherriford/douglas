mod bract_path_factory;
mod config;
mod constants;
mod file_logger;
mod init_command;
mod response_formatter;
mod start_bract_command;
#[macro_use]
mod macros;

use bract::{Client, client::Request};
use bract_path_factory::BractPathFactory;
use clap::{Parser, Subcommand, ValueEnum};
use config::LocalConfigRepository;
use credentials::{Credentials, create_for_target};
use file_system::{
    FileReader, FileWriter, Folder, Links, LocalFileAppender, LocalFileDeleter, LocalFileReader,
    LocalFileWriter, LocalFolder, LocalLinks, LocalPermissions, LocalUnixDomainSocket, Permissions,
};
use init_command::InitCommand;
use log::{Logger, StdOutLogger};
use os::{Os, Unix};
use response_formatter::{JsonResponseFormatter, PlainResponseFormatter, ResponseFormatter};
use start_bract_command::StartBractCommand;
use std::{path::Path, sync::Arc};
use tokio::runtime::Runtime;

#[derive(ValueEnum, Clone, Debug)]
enum OutputStyle {
    Plain,
    Json,
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

#[derive(Debug)]
pub enum Response {
    BractResponse(bract::Response),
    Okay,
    Error(String),
}

impl From<bract::Response> for Response {
    fn from(f: bract::Response) -> Self {
        Response::BractResponse(f)
    }
}

fn main() {
    let stdout_log = Arc::new(StdOutLogger::new());
    let os = Arc::new(Unix::new());
    let credentials = create_for_target(os.clone());
    let folder = Arc::new(LocalFolder::new());
    let permissions = Arc::new(LocalPermissions::new());
    let file_reader = Arc::new(LocalFileReader::new());
    let file_writer = Arc::new(LocalFileWriter::new());
    let config_repository = Arc::new(LocalConfigRepository::new(
        folder.clone(),
        permissions.clone(),
        file_reader.clone(),
        file_writer.clone(),
    ));
    let bract_path_factory = Arc::new(BractPathFactory::new(folder.clone()));
    let cli = Cli::parse();
    let bract_response_formatter: Box<dyn ResponseFormatter> = match cli.output_style {
        OutputStyle::Plain => Box::new(PlainResponseFormatter::new()),
        OutputStyle::Json => Box::new(JsonResponseFormatter::new()),
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
            println!("🌲 Initializing douglas…");
            let success = init(
                &bract_response_formatter,
                stdout_log.clone(),
                credentials.clone(),
                folder.clone(),
                permissions.clone(),
                config_repository,
                service_user,
                service_group,
                mount_root_path,
                log_path,
                docker_socket_path,
            );
            if success {
                let log_to_std_out = !daemonize;
                start_bract(
                    bract_response_formatter,
                    stdout_log,
                    os,
                    credentials,
                    folder,
                    permissions,
                    file_reader,
                    file_writer,
                    bract_path_factory,
                    daemonize,
                    log_to_std_out,
                );
            }
        }
        Commands::Start { daemonize } => {
            let log_to_std_out = !daemonize;
            start_bract(
                bract_response_formatter,
                stdout_log,
                os,
                credentials,
                folder,
                permissions,
                file_reader,
                file_writer,
                bract_path_factory,
                daemonize,
                log_to_std_out,
            );
        }
        Commands::Status => {
            let socket_path = or_print_and_exit_with_error!(bract_path_factory.bract_socket_path());
            let token_path = or_print_and_exit_with_error!(bract_path_factory.token_path());
            status(
                bract_response_formatter,
                stdout_log,
                file_reader,
                socket_path,
                token_path,
            );
        }
        Commands::Shutdown => {
            let socket_path = or_print_and_exit_with_error!(bract_path_factory.bract_socket_path());
            let token_path = or_print_and_exit_with_error!(bract_path_factory.token_path());
            shutdown(
                bract_response_formatter,
                stdout_log,
                file_reader,
                socket_path,
                token_path,
            );
        }
    }
}

fn init(
    response_formatter: &Box<dyn ResponseFormatter>,
    stdout_log: Arc<StdOutLogger>,
    credentials: Arc<dyn Credentials + Send + Sync>,
    folder: Arc<LocalFolder>,
    permissions: Arc<LocalPermissions>,
    config_repository: Arc<LocalConfigRepository>,
    service_user: String,
    service_group: String,
    mount_root_path: String,
    log_path: String,
    docker_socket_path: String,
) -> bool {
    let mount_root_path = Path::new(&mount_root_path);
    let log_path = Path::new(&log_path);
    let docker_socket_path = Path::new(&docker_socket_path);

    let result = InitCommand::new(
        stdout_log.clone(),
        &service_user,
        &service_group,
        &mount_root_path,
        &log_path,
        &docker_socket_path,
        credentials.clone(),
        folder.clone(),
        permissions.clone(),
        config_repository.clone(),
    )
    .run();

    match result {
        Ok(()) => {
            println!("{}", response_formatter.format(Response::Okay));
            true
        }
        Err(err) => {
            eprintln!(
                "{:?}",
                response_formatter.format(Response::Error(format!("{:?}", err)))
            );
            false
        }
    }
}

fn start_bract(
    response_formatter: Box<dyn ResponseFormatter>,
    stdout_log: Arc<dyn Logger + Send + Sync + 'static>,
    os: Arc<dyn Os + Send + Sync + 'static>,
    credentials: Arc<dyn Credentials + Send + Sync>,
    folder: Arc<dyn Folder + Send + Sync + 'static>,
    permissions: Arc<dyn Permissions + Send + Sync + 'static>,
    file_reader: Arc<dyn FileReader + Send + Sync + 'static>,
    file_writer: Arc<dyn FileWriter + Send + Sync + 'static>,
    bract_path_factory: Arc<BractPathFactory>,
    daemonize: bool,
    log_to_std_out: bool,
) {
    let config_respository = Arc::new(LocalConfigRepository::new(
        Arc::clone(&folder),
        Arc::clone(&permissions),
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
    ));
    let file_deleter = Arc::new(LocalFileDeleter::new());
    let file_appender = Arc::new(LocalFileAppender::new());
    let links: Arc<dyn Links + Send + Sync + 'static> = Arc::new(LocalLinks::new());
    let unix_domain_socket = Arc::new(LocalUnixDomainSocket::new());

    let override_logger: Option<Arc<dyn Logger + Send + Sync + 'static>> = if log_to_std_out {
        Some(Arc::clone(&stdout_log))
    } else {
        None
    };

    let start_bract = StartBractCommand::new(
        Arc::clone(&credentials),
        Arc::clone(&folder),
        config_respository,
        Arc::clone(&file_reader),
        Arc::clone(&file_writer),
        file_deleter,
        file_appender,
        Arc::clone(&links),
        Arc::clone(&os),
        Arc::clone(&permissions),
        unix_domain_socket,
        daemonize,
        Arc::clone(&bract_path_factory),
    );

    stdout_log.info("Starting Bract…");
    match start_bract.run(override_logger) {
        Ok(_) => println!(
            "{}",
            response_formatter.format(bract::Response::Stopped.into())
        ),
        Err(err) => println!(
            "{}",
            response_formatter.format(Response::Error(format!("{}", err)))
        ),
    }
}

fn status(
    response_formatter: Box<dyn ResponseFormatter>,
    stdout_log: Arc<StdOutLogger>,
    file_reader: Arc<LocalFileReader>,
    socket_path: std::path::PathBuf,
    token_path: std::path::PathBuf,
) {
    let client = Client::new(
        stdout_log,
        file_reader,
        socket_path.as_path(),
        token_path.as_path(),
    );

    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let response = or_print_and_exit_with_error!(client.request(Request::Status).await);
        println!("{}", response_formatter.format(response.into()));
    });
}

fn shutdown(
    response_formatter: Box<dyn ResponseFormatter>,
    stdout_log: Arc<StdOutLogger>,
    file_reader: Arc<LocalFileReader>,
    socket_path: std::path::PathBuf,
    token_path: std::path::PathBuf,
) {
    let client = Client::new(
        stdout_log,
        file_reader,
        socket_path.as_path(),
        token_path.as_path(),
    );

    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let response = or_print_and_exit_with_error!(client.request(Request::Shutdown).await);
        println!("{}", response_formatter.format(response.into()));
    });
}
