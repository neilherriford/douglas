mod bootstrap;
#[macro_use]
pub(crate) mod macros;
mod cli_reporter;

use crate::cli_reporter::CliReporter;
use ::config::DouglasFolders;
use bract_client::Client;
use clap::{Parser, Subcommand, ValueEnum};
use credentials::create_credentials;
use crossterm::style::Stylize;
use daemonize::Daemonize;
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Inspect, Permissions,
    UnixFileDeleter, UnixFileReader, UnixFileWriter, UnixFolder, UnixInspect, UnixPermissions,
};
use identity::{Identity, LocalIdentity};
use log::{BufferedFileReporter, Reporter, Span, TeeReporter};
use os::{EnvironmentVariableReader, Os, Unix, UnixEnvironmentVariableReader};
use resin_client::ClientBuilder as _;
use std::{
    collections::{HashMap, HashSet},
    fmt::{Debug, Display},
    io,
    path::{Path, PathBuf},
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
        help = "Set the output mode. Defaults to plain text for most commands; `start` defaults \
                to an interactive TUI instead and only switches to plain/json output when this \
                is explicitly set, since plain/json mode has no live terminal to render into."
    )]
    output_style: Option<OutputStyle>,
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
    #[command(about = "Report the status of every seedling and of OpenBao's secrets state")]
    Status,
    #[command(hide = true)]
    Service {
        #[command(subcommand)]
        service: ServiceCommand,
    },
    #[command(about = "Seedling commands")]
    Seedling {
        #[command(subcommand)]
        seedling: SeedlingCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceCommand {
    Bract {
        #[arg(long, help = "File descriptor to stream boot information to")]
        notify_fd: i32,
    },
    Resin {
        #[arg(
            long,
            default_value_t = false,
            help = "Debug mode — runs in foreground with TUI, no pipe required"
        )]
        dbg: bool,
        #[arg(
            long,
            help = "File descriptor to stream boot information to",
            required_unless_present = "dbg"
        )]
        notify_fd: Option<i32>,
    },
    Seedbank {
        #[arg(
            long,
            default_value_t = false,
            help = "Debug mode — runs in foreground with TUI, no pipe required"
        )]
        dbg: bool,
        #[arg(
            long,
            help = "File descriptor to stream boot information to",
            required_unless_present = "dbg"
        )]
        notify_fd: Option<i32>,
    },
}

#[derive(Subcommand, Debug)]
enum SeedlingCommand {
    #[command(about = "Query seedling status")]
    Status {
        #[arg(long, help = "The seedling to to query")]
        name: String,
    },
    #[command(about = "Stop a running seedling")]
    Stop {
        #[arg(long, help = "The seedling to to stop")]
        name: String,
    },
    #[command(about = "Delete a stopped seedling")]
    Drop {
        #[arg(long, help = "The seedling to to drop")]
        name: String,
    },
    #[command(about = "Start a seedling")]
    Start {
        #[arg(long, help = "The seedling to to start")]
        name: String,
    },
    #[command(about = "Create a new seedling")]
    New {
        #[arg(long, help = "The seedling to to create")]
        name: String,
        #[arg(
            long,
            help = "Path to a TOML seedling spec file; reads stdin if omitted"
        )]
        file: Option<PathBuf>,
    },
    #[command(about = "Create a blank template for a seedling")]
    CreateTemplate,
    #[command(
        about = "Find and remove orphaned containers, networks, route files, and resin repositories"
    )]
    Prune {
        #[arg(
            long,
            default_value_t = false,
            help = "Skip the confirmation prompt and prune immediately"
        )]
        yes: bool,
    },
}

impl Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::Start { .. } => f.write_str("start"),
            Commands::Stop => f.write_str("stop"),
            Commands::Status => f.write_str("status"),
            Commands::Service {
                service: ServiceCommand::Bract { .. },
            } => f.write_str("service bract"),
            Commands::Service {
                service: ServiceCommand::Resin { .. },
            } => f.write_str("service resin"),
            Commands::Service {
                service: ServiceCommand::Seedbank { .. },
            } => f.write_str("service seedbank"),
            Commands::Seedling {
                seedling: SeedlingCommand::Status { .. },
            } => f.write_str("seedling status"),
            Commands::Seedling {
                seedling: SeedlingCommand::Start { .. },
            } => f.write_str("start seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::Stop { .. },
            } => f.write_str("stop seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::Drop { .. },
            } => f.write_str("drop seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::New { .. },
            } => f.write_str("create seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::CreateTemplate,
            } => f.write_str("create seedling template"),
            Commands::Seedling {
                seedling: SeedlingCommand::Prune { .. },
            } => f.write_str("prune orphans"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output_style_arg = cli.output_style;
    let output_style = output_style_arg.unwrap_or(OutputStyle::Plain);

    match cli.command {
        Commands::Start { plan_only } => run_with_tokio(start(plan_only, output_style_arg)),
        Commands::Stop => todo!(),
        Commands::Status => run_with_tokio(status(output_style)),
        Commands::Service {
            service: ServiceCommand::Bract { notify_fd },
        } => start_bract(notify_fd),
        Commands::Service {
            service: ServiceCommand::Resin { dbg: true, .. },
        } => run_with_tokio(resin_debug_mode()),
        Commands::Service {
            service:
                ServiceCommand::Resin {
                    notify_fd: Some(fd),
                    ..
                },
        } => start_resin(fd),
        Commands::Service {
            service: ServiceCommand::Seedbank { dbg: true, .. },
        } => run_with_tokio(seedbank_debug_mode()),
        Commands::Service {
            service:
                ServiceCommand::Seedbank {
                    notify_fd: Some(fd),
                    ..
                },
        } => start_seedbank(fd),
        Commands::Service {
            service: ServiceCommand::Resin { .. } | ServiceCommand::Seedbank { .. },
        } => unreachable!("clap requires --notify-fd when --dbg is not set"),
        Commands::Seedling {
            seedling: SeedlingCommand::Status { name },
        } => run_with_tokio(get_seedling_status(&name, output_style)),
        Commands::Seedling {
            seedling: SeedlingCommand::Start { name },
        } => run_with_tokio(run_seedling_action(
            &name,
            output_style,
            SeedlingAction::Start,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::Stop { name },
        } => run_with_tokio(run_seedling_action(
            &name,
            output_style,
            SeedlingAction::Stop,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::Drop { name },
        } => run_with_tokio(run_seedling_action(
            &name,
            output_style,
            SeedlingAction::Drop,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::New { name, file },
        } => run_with_tokio(create_seedling(&name, file.as_deref(), output_style)),
        Commands::Seedling {
            seedling: SeedlingCommand::CreateTemplate,
        } => create_seedling_template(output_style),
        Commands::Seedling {
            seedling: SeedlingCommand::Prune { yes },
        } => run_with_tokio(prune_orphans(output_style, yes)),
    }
}

fn example_name(value: &str) -> seedbank_types::Name {
    value
        .parse()
        .unwrap_or_else(|_| unreachable!("'{value}' is a valid seedling name literal"))
}

fn example_seedling_spec() -> seedbank_types::SeedlingSpec {
    let mut mounts = HashMap::new();

    mounts.insert(
        example_name("config"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::Persisted,
            PathBuf::from("/etc/example/config"),
            seedbank_types::AccessMode::ReadOnly,
            HashSet::new(),
        ),
    );

    mounts.insert(
        example_name("cache"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::InMemory,
            PathBuf::from("/var/cache/example"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    mounts.insert(
        example_name("shared-assets"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::PersistedShared(vec![
                example_name("sibling-a"),
                example_name("sibling-b"),
            ]),
            PathBuf::from("/var/lib/example/shared"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    seedbank_types::SeedlingSpec::new(
        mounts,
        seedbank_types::PortSpec {
            public: 8080,
            additional: vec![seedbank_types::PortMapping {
                external: 1234,
                internal: 4321,
            }],
        },
    )
}

fn create_seedling_template(output_style: OutputStyle) -> ExitCode {
    let toml = match toml::to_string_pretty(&example_seedling_spec()) {
        Ok(toml) => toml,
        Err(err) => {
            eprintln!("{}", format!("Could not render template: {err}").red());
            return ExitCode::from(1);
        }
    };

    match output_style {
        OutputStyle::Plain => println!("{toml}"),
        OutputStyle::Json => match serde_json::to_string(&serde_json::json!({ "toml": toml })) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!(
                    "{}",
                    format!("Could not serialize template as JSON: {err}").red()
                );
                return ExitCode::from(1);
            }
        },
    }

    ExitCode::from(0)
}

fn read_seedling_spec_input(
    file_reader: &dyn FileReader,
    file: Option<&Path>,
) -> Result<String, FileSystemError> {
    match file {
        Some(path) => file_reader.read_all(path),
        None => file_reader.read_stdin(),
    }
}

async fn create_seedling(name: &str, file: Option<&Path>, output_style: OutputStyle) -> ExitCode {
    let (douglas_folders, guard) = seedling_command_context("Creating seedling");

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let client = bract_client::UdsClient::new(guard.reporter(), &douglas_folders);

    let file_reader = UnixFileReader::new();

    let input = match read_seedling_spec_input(&file_reader, file) {
        Ok(input) => input,
        Err(err) => {
            print_error(
                output_style,
                &format!("Could not read seedling spec: {err}"),
            );
            return ExitCode::from(1);
        }
    };

    let spec: seedbank_types::SeedlingSpec = match toml::from_str(&input) {
        Ok(spec) => spec,
        Err(err) => {
            print_error(output_style, &format!("Invalid seedling spec:\n\n{err}"));
            return ExitCode::from(1);
        }
    };

    match client.new_seedling(&seedling_name, &spec).await {
        Ok(message) => {
            match output_style {
                OutputStyle::Plain => println!("{message}"),
                OutputStyle::Json => {
                    match serde_json::to_string(
                        &serde_json::json!({ "success": true, "message": message }),
                    ) {
                        Ok(json) => println!("{json}"),
                        Err(err) => {
                            print_error(
                                output_style,
                                &format!("Could not serialize response as JSON: {err}"),
                            );
                            return ExitCode::from(1);
                        }
                    }
                }
            }
            guard.finish_with_outcome(log::Outcome::Ok);
            ExitCode::from(0)
        }
        Err(err) => {
            let message = format!("Could not create seedling: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            ExitCode::from(1)
        }
    }
}

// Daemonize redirects stdout/stderr to `/dev/null` by default, lets
// keep logs for crashes that happen pre-fork.
fn open_daemon_crash_log(path: &Path) -> Option<(std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "Failed to create crash log directory '{}': {err}",
            parent.display()
        );
        return None;
    }

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .inspect_err(|err| eprintln!("Failed to open crash log '{}': {err}", path.display()))
        .ok()?;
    let stderr = stdout.try_clone().ok()?;
    Some((stdout, stderr))
}

// `Daemonize`'s `.user()/.group()` only call bare `setgid`/`setuid`
// it never calls `initgroups`, so the daemonized child keeps whatever
// supplementary groups the root parent process happened to have
// instead of the target user's actual groups.
//
// That silently breaks anything relying on group membership after the
// privilege drop (e.g. `chown`ing a file to a group the user was
// added to but the process was never told about).
//
// This must run via `Daemonize::privileged_action`, which fires
// before `setgid`/`setuid` while still root, the standard order:
//   `initgroups` → `setgid` → `setuid`
fn initgroups_for(user_name: &str) {
    if let Err(err) = try_initgroups_for(user_name) {
        eprintln!("Failed to initialize supplementary groups for '{user_name}': {err}");
    }
}

fn try_initgroups_for(user_name: &str) -> Result<(), String> {
    let Some(user) = users::get_user_by_name(user_name) else {
        return Err(format!("user '{user_name}' not found"));
    };
    let c_user = std::ffi::CString::new(user_name).map_err(|err| err.to_string())?;
    let gid = nix::unistd::Gid::from_raw(user.primary_group_id());

    platform_initgroups(&c_user, gid).map_err(|err| err.to_string())
}

#[cfg(target_os = "linux")]
fn platform_initgroups(user: &std::ffi::CStr, gid: nix::unistd::Gid) -> nix::Result<()> {
    nix::unistd::initgroups(user, gid)
}

// There's no initgroups on macOS, opendirectoryd is used there. Kept
// `Result`-returning (rather than `()`) so this matches the Linux
// implementation's signature for the shared call site.
#[cfg(target_os = "macos")]
#[allow(clippy::unnecessary_wraps)]
fn platform_initgroups(_user: &std::ffi::CStr, _gid: nix::unistd::Gid) -> nix::Result<()> {
    Ok(())
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

fn start_bract(reporting_fd: i32) -> ExitCode {
    let mut daemonize = Daemonize::new();
    let crash_log_path = DouglasFolders::new()
        .log_dir("bract")
        .join("bract.crash.log");
    if let Some((stdout, stderr)) = open_daemon_crash_log(&crash_log_path) {
        daemonize = daemonize.stdout(stdout).stderr(stderr);
    }

    match daemonize.start() {
        Ok(()) => run_with_tokio(run_bract_server(reporting_fd)),
        Err(err) => {
            eprintln!("Failed to daemonize bract server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_bract_server(reporting_fd: i32) -> ExitCode {
    let bract = match bract::Bract::build(reporting_fd).await {
        Ok(bract) => Arc::new(bract),
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

fn start_resin(reporting_fd: i32) -> ExitCode {
    let mut daemonize = Daemonize::new()
        .user(resin::DOUGLAS_RESIN_USER)
        .group(resin::DOUGLAS_RESIN_GROUP)
        .privileged_action(|| initgroups_for(resin::DOUGLAS_RESIN_USER));
    let crash_log_path = DouglasFolders::new()
        .log_dir(resin::RESIN)
        .join(format!("{}.crash.log", resin::RESIN));
    if let Some((stdout, stderr)) = open_daemon_crash_log(&crash_log_path) {
        daemonize = daemonize.stdout(stdout).stderr(stderr);
    }

    match daemonize.start() {
        Ok(()) => run_with_tokio(run_resin_server(reporting_fd)),
        Err(err) => {
            eprintln!("Failed to daemonize resin server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_resin_server(reporting_fd: i32) -> ExitCode {
    let Ok(server) = resin::Server::build(Some(reporting_fd), resin_types::DEFAULT_PORT).await
    else {
        return ExitCode::from(1);
    };

    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(_) => ExitCode::from(1),
    }
}

fn start_seedbank(reporting_fd: i32) -> ExitCode {
    let mut daemonize = Daemonize::new()
        .user(seedbank::DOUGLAS_SEEDBANK_USER)
        .group(seedbank::DOUGLAS_SEEDBANK_GROUP)
        .privileged_action(|| initgroups_for(seedbank::DOUGLAS_SEEDBANK_USER));
    let crash_log_path = DouglasFolders::new()
        .log_dir(seedbank::SEEDBANK)
        .join(format!("{}.crash.log", seedbank::SEEDBANK));
    if let Some((stdout, stderr)) = open_daemon_crash_log(&crash_log_path) {
        daemonize = daemonize.stdout(stdout).stderr(stderr);
    }

    match daemonize.start() {
        Ok(()) => run_with_tokio(run_seedbank_server(reporting_fd)),
        Err(err) => {
            eprintln!("Failed to daemonize seedbank server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_seedbank_server(reporting_fd: i32) -> ExitCode {
    let server = match seedbank::Server::build(Some(reporting_fd)).await {
        Ok(server) => Arc::new(server),
        Err(err) => {
            eprintln!("Failed to start seedbank: {err:?}");
            return ExitCode::from(1);
        }
    };

    if let Err(err) = server.start().await {
        eprintln!("Failed to start seedbank: {err:?}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}

async fn seedbank_debug_mode() -> ExitCode {
    let server = match seedbank::Server::build(None).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("seedbank: failed to start: {e}");
            return ExitCode::from(1);
        }
    };

    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("seedbank: {e}");
            ExitCode::from(1)
        }
    }
}

async fn resin_debug_mode() -> ExitCode {
    let server = match resin::Server::build(None, resin_types::DEFAULT_PORT).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("resin: failed to start: {e}");
            return ExitCode::from(1);
        }
    };

    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("resin: {e}");
            ExitCode::from(1)
        }
    }
}

fn build_cli_reporter(
    douglas_folders: &DouglasFolders,
    log_name: &str,
) -> io::Result<Arc<dyn Reporter>> {
    let cli_reporter = CliReporter::start()?;
    Ok(Arc::new(TeeReporter::new(vec![
        Box::new(BufferedFileReporter::new(
            douglas_folders.log_file(log_name),
        )),
        Box::new(cli_reporter),
    ])))
}

fn build_plain_reporter(douglas_folders: &DouglasFolders, log_name: &str) -> Arc<dyn Reporter> {
    Arc::new(BufferedFileReporter::new(
        douglas_folders.log_file(log_name),
    ))
}

async fn start(plan_only: bool, output_style: Option<OutputStyle>) -> ExitCode {
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
        credentials,
        permissions,
        environment_variable_reader,
        folder,
        os,
        douglas_folders.clone(),
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
        inspect,
        openbao_client_factory,
        Arc::clone(&bract_client),
        openbao_file_reader,
        openbao_file_writer,
        openbao_file_deleter,
        Arc::new(UnixPermissions::new()),
        &mut identity,
        &douglas_folders,
    )
    .await;

    if !succeeded {
        if let Some(style) = output_style {
            print_error(style, "OpenBao bootstrap failed");
        }
        return ExitCode::from(1);
    }

    log_orphans_if_any(&reporter, bract_client.as_ref()).await;

    if let Some(style) = output_style {
        print_start_result(style);
    }

    ExitCode::from(0)
}

#[derive(serde::Serialize)]
struct SeedlingStatusEntry {
    name: String,
    status: String,
}

#[derive(serde::Serialize)]
struct CoreServiceStatus {
    name: String,
    running: bool,
    detail: String,
}

#[derive(serde::Serialize)]
struct StatusReport {
    seedlings: Vec<SeedlingStatusEntry>,
    seedlings_error: Option<String>,
    core_services: Vec<CoreServiceStatus>,
    traefik_routes: Vec<String>,
    traefik_routes_error: Option<String>,
    openbao: Option<bract_types::OpenBaoReport>,
    openbao_error: Option<String>,
}

fn bract_error_means_unreachable(err: &bract_client::Error) -> bool {
    matches!(
        err,
        bract_client::Error::MissingSocket
            | bract_client::Error::ConnectionRefused
            | bract_client::Error::NoResponse
            | bract_client::Error::IoError(_)
    )
}

fn probe_bract(
    seedling_names_result: &Result<Vec<seedbank_types::Name>, bract_client::Error>,
) -> CoreServiceStatus {
    let running = !matches!(seedling_names_result, Err(err) if bract_error_means_unreachable(err));
    let detail = match seedling_names_result {
        Ok(names) => format!("{} seedling(s) registered", names.len()),
        Err(err) => err.to_string(),
    };
    CoreServiceStatus {
        name: "bract".to_string(),
        running,
        detail,
    }
}

fn probe_seedbank(
    seedling_names_result: &Result<Vec<seedbank_types::Name>, bract_client::Error>,
) -> CoreServiceStatus {
    let (running, detail) = match seedling_names_result {
        Ok(names) => (true, format!("{} seedling(s) registered", names.len())),
        Err(err) if bract_error_means_unreachable(err) => (
            false,
            "bract is unreachable, cannot determine seedbank status".to_string(),
        ),
        Err(err) => (false, err.to_string()),
    };
    CoreServiceStatus {
        name: "seedbank".to_string(),
        running,
        detail,
    }
}

async fn probe_resin(reporter: Arc<dyn Reporter>) -> CoreServiceStatus {
    let (running, detail) = match resin_client::LocalhostClientBuilder.build(reporter).await {
        Ok(mut client) => match client.list_repositories().await {
            Ok(repositories) => (true, format!("{} repositories", repositories.len())),
            Err(err) => (false, err.to_string()),
        },
        Err(err) => (false, err.to_string()),
    };
    CoreServiceStatus {
        name: "resin".to_string(),
        running,
        detail,
    }
}

fn list_traefik_routes(
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
) -> Result<Vec<String>, String> {
    let dynamic_dir = bract::traefik_dynamic_dir(douglas_folders).map_err(|err| err.to_string())?;

    if !folder.exists(&dynamic_dir) {
        return Ok(Vec::new());
    }

    folder
        .entries(&dynamic_dir)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.name.strip_suffix(".yml"))
                .map(std::string::ToString::to_string)
                .collect()
        })
        .map_err(|err| err.to_string())
}

async fn status(output_style: OutputStyle) -> ExitCode {
    let douglas_folders = DouglasFolders::new();
    let reporter = build_plain_reporter(&douglas_folders, "douglas-cli");
    let guard = Span::new(Arc::clone(&reporter), "Status", log::ScopeKind::Task).start_guard();

    let bract_client = bract_client::UdsClient::new(Arc::clone(&reporter), &douglas_folders);

    let seedling_names_result = bract_client.list_seedlings().await;
    let (seedlings, seedlings_error) = match &seedling_names_result {
        Ok(names) => {
            let mut entries = Vec::with_capacity(names.len());
            for name in names {
                let status = match bract_client.seedling_status(name).await {
                    Ok(status) => status.to_string(),
                    Err(err) => format!("unavailable ({err})"),
                };
                entries.push(SeedlingStatusEntry {
                    name: name.to_string(),
                    status,
                });
            }
            (entries, None)
        }
        Err(err) => (Vec::new(), Some(err.to_string())),
    };

    let core_services = vec![
        probe_bract(&seedling_names_result),
        probe_resin(Arc::clone(&reporter)).await,
        probe_seedbank(&seedling_names_result),
    ];

    let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
    let (traefik_routes, traefik_routes_error) =
        match list_traefik_routes(folder.as_ref(), &douglas_folders) {
            Ok(routes) => (routes, None),
            Err(err) => (Vec::new(), Some(err)),
        };

    let (openbao_report, openbao_error) = match bract_client.openbao_status().await {
        Ok(report) => (Some(report), None),
        Err(err) => (None, Some(err.to_string())),
    };

    print_status_report(
        output_style,
        &StatusReport {
            seedlings,
            seedlings_error,
            core_services,
            traefik_routes,
            traefik_routes_error,
            openbao: openbao_report,
            openbao_error,
        },
    );

    guard.finish_with_outcome(log::Outcome::Ok);
    ExitCode::from(0)
}

fn print_status_report(output_style: OutputStyle, report: &StatusReport) {
    match output_style {
        OutputStyle::Json => {
            if let Ok(json) = serde_json::to_string(report) {
                println!("{json}");
            }
        }
        OutputStyle::Plain => {
            println!("Core services:");
            for service in &report.core_services {
                let state = if service.running {
                    "running"
                } else {
                    "unavailable"
                };
                println!("  {}: {} ({})", service.name, state, service.detail);
            }

            println!("Seedlings:");
            if let Some(err) = &report.seedlings_error {
                println!("  unavailable ({err})");
            } else if report.seedlings.is_empty() {
                println!("  none");
            } else {
                for entry in &report.seedlings {
                    println!("  {}: {}", entry.name, entry.status);
                }
            }

            println!("Traefik:");
            if let Some(err) = &report.traefik_routes_error {
                println!("  routes unavailable ({err})");
            } else if report.traefik_routes.is_empty() {
                println!("  routes: none");
            } else {
                println!("  routes:");
                for route in &report.traefik_routes {
                    println!("    {route}");
                }
            }

            println!("OpenBao:");
            match (&report.openbao, &report.openbao_error) {
                (Some(openbao), _) => print_openbao_report_plain(openbao),
                (None, Some(err)) => println!("  unavailable ({err})"),
                (None, None) => println!("  unavailable"),
            }
        }
    }
}

fn print_openbao_report_plain(report: &bract_types::OpenBaoReport) {
    println!("  running: {}", report.is_running);
    if !report.is_running {
        return;
    }
    println!("  initialized: {}", report.is_initialized);
    println!("  sealed: {}", report.is_sealed);
    println!("  credentials available: {}", report.credentials_available);
    println!("  credentials work: {}", report.credentials_work);
    if !report.credentials_work {
        return;
    }
    println!("  approle enabled: {}", report.app_role_enabled);
    println!("  acme enabled: {}", report.acme_enabled);
    println!("  root ca configured: {}", report.root_ca_configured);
    println!("  acme pki role created: {}", report.acme_pki_role_created);
    println!("  mounts:");
    if report.mounts.is_empty() {
        println!("    none");
    } else {
        for (path, kind) in &report.mounts {
            println!("    {path} ({kind})");
        }
    }
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

async fn log_orphans_if_any(reporter: &Arc<dyn Reporter>, bract_client: &dyn bract_client::Client) {
    let guard = Span::new(
        Arc::clone(reporter),
        "Checking for orphaned resources",
        log::ScopeKind::Task,
    )
    .start_guard();

    match bract_client.find_orphans().await {
        Ok(orphans) if orphans.is_empty() => guard.finish_with_outcome(log::Outcome::Ok),
        Ok(orphans) => {
            let details = [
                ("container", names(&orphans.containers)),
                ("network", names(&orphans.networks)),
                ("route file", names(&orphans.route_files)),
                ("resin repository", orphans.resin_repositories.clone()),
                ("mount", names(&orphans.mounts)),
                ("openbao secret", names(&orphans.openbao_secrets)),
            ]
            .into_iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(label, names)| format!("{label}(s): {}", names.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");

            guard.span().message(
                log::Level::Warn,
                &format!(
                    "Found orphaned resources (run `douglas seedling prune` to clean up): {details}"
                ),
            );
            guard.finish_with_outcome(log::Outcome::Ok);
        }
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Could not check for orphaned resources: {err}"),
            );
            guard.finish_with_outcome(log::Outcome::Failed);
        }
    }
}

fn names(items: &[seedbank_types::Name]) -> Vec<String> {
    items.iter().map(std::string::ToString::to_string).collect()
}

fn seedling_command_context(label: &str) -> (DouglasFolders, log::ScopeGuard) {
    let douglas_folders = DouglasFolders::new();
    let reporter = build_plain_reporter(&douglas_folders, "douglas-cli");
    let guard = Span::new(reporter, label, log::ScopeKind::Task).start_guard();
    (douglas_folders, guard)
}

#[derive(serde::Serialize)]
struct JsonStatusResponse<'a> {
    status: Option<&'a bract_types::SeedlingStatus>,
    success: bool,
    error_message: Option<String>,
}

fn print_status_json(status: &bract_types::SeedlingStatus) -> Result<(), serde_json::Error> {
    let response = JsonStatusResponse {
        status: Some(status),
        success: true,
        error_message: None,
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn print_error(output_style: OutputStyle, message: &str) {
    match output_style {
        OutputStyle::Plain => eprintln!("{}", message.red()),
        OutputStyle::Json => {
            let response = JsonStatusResponse {
                status: None,
                success: false,
                error_message: Some(message.to_string()),
            };
            if let Ok(json) = serde_json::to_string(&response) {
                println!("{json}");
            }
        }
    }
}

fn parse_seedling_name(
    guard: &log::ScopeGuard,
    output_style: OutputStyle,
    name: &str,
) -> Option<seedbank_types::Name> {
    if let Ok(name) = name.parse() {
        Some(name)
    } else {
        guard
            .span()
            .message(log::Level::Warn, "Invalid seedling name");
        print_error(output_style, "Invalid seedling name");
        None
    }
}

async fn get_seedling_status(name: &str, output_style: OutputStyle) -> ExitCode {
    let (douglas_folders, guard) = seedling_command_context("Fetching status");

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let result = fetch_status(&guard, &douglas_folders, output_style, seedling_name).await;

    if result == ExitCode::SUCCESS {
        guard.finish_with_outcome(log::Outcome::Ok);
    }

    result
}

async fn fetch_status(
    guard: &log::ScopeGuard,
    douglas_folders: &DouglasFolders,
    output_style: OutputStyle,
    seedling_name: seedbank_types::Name,
) -> ExitCode {
    let client = bract_client::UdsClient::new(guard.reporter(), douglas_folders);

    match client.seedling_status(&seedling_name).await {
        Ok(status) => match output_style {
            OutputStyle::Plain => println!("{status}"),
            OutputStyle::Json => {
                if let Err(err) = print_status_json(&status) {
                    let message = format!("Could not serialize status as JSON: {err}");
                    guard.span().message(log::Level::Warn, &message);
                    print_error(output_style, &err.to_string());
                    return ExitCode::from(1);
                }
            }
        },
        Err(err) => {
            let message = format!("Could not determine seedling status: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    ExitCode::from(0)
}

enum SeedlingAction {
    Start,
    Stop,
    Drop,
}

impl SeedlingAction {
    fn label(&self) -> &'static str {
        match self {
            SeedlingAction::Start => "Starting seedling",
            SeedlingAction::Stop => "Stopping seedling",
            SeedlingAction::Drop => "Dropping seedling",
        }
    }

    fn error_prefix(&self) -> &'static str {
        match self {
            SeedlingAction::Start => "Could not start seedling",
            SeedlingAction::Stop => "Could not stop seedling",
            SeedlingAction::Drop => "Could not drop seedling",
        }
    }

    async fn invoke(
        &self,
        client: &dyn bract_client::Client,
        name: &seedbank_types::Name,
    ) -> Result<(), bract_client::Error> {
        match self {
            SeedlingAction::Start => client.start_seedling(name).await,
            SeedlingAction::Stop => client.stop_seedling(name).await,
            SeedlingAction::Drop => client.drop_seedling(name).await,
        }
    }
}

async fn run_seedling_action(
    name: &str,
    output_style: OutputStyle,
    action: SeedlingAction,
) -> ExitCode {
    let (douglas_folders, guard) = seedling_command_context(action.label());

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let client = bract_client::UdsClient::new(guard.reporter(), &douglas_folders);

    match action.invoke(&client, &seedling_name).await {
        Ok(()) => {
            let status_result =
                fetch_status(&guard, &douglas_folders, output_style, seedling_name).await;

            if status_result != ExitCode::SUCCESS {
                return status_result;
            }
        }
        Err(err) => {
            let message = format!("{}: {err}", action.error_prefix());
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    guard.finish_with_outcome(log::Outcome::Ok);

    ExitCode::from(0)
}

fn print_orphans(orphans: &bract_types::Orphans) {
    print_orphan_group("Containers", &orphans.containers);
    print_orphan_group("Networks", &orphans.networks);
    print_orphan_group("Route files", &orphans.route_files);
    print_orphan_group("Resin repositories", &orphans.resin_repositories);
    print_orphan_group("Mounts", &orphans.mounts);
    print_orphan_group("OpenBao secrets", &orphans.openbao_secrets);
}

fn print_orphan_group<T: std::fmt::Display>(label: &str, names: &[T]) {
    if names.is_empty() {
        return;
    }
    println!("{}", label.bold());
    for name in names {
        println!("  - {name}");
    }
}

fn confirm_prune() -> bool {
    print!("Prune the above? [y/N] ");
    if io::Write::flush(&mut io::stdout()).is_err() {
        return false;
    }

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }

    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

async fn prune_orphans(output_style: OutputStyle, skip_confirmation: bool) -> ExitCode {
    let (douglas_folders, guard) = seedling_command_context("Finding orphans");
    let client = bract_client::UdsClient::new(guard.reporter(), &douglas_folders);

    let orphans = match client.find_orphans().await {
        Ok(orphans) => orphans,
        Err(err) => {
            let message = format!("Could not find orphans: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    };

    if orphans.is_empty() {
        match output_style {
            OutputStyle::Plain => println!("No orphans found."),
            OutputStyle::Json => println!(
                "{}",
                serde_json::json!({ "orphans": orphans, "pruned": false })
            ),
        }
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    let has_prunable = !orphans.is_empty();

    if let OutputStyle::Plain = output_style {
        print_orphans(&orphans);
    }

    if !has_prunable {
        if let OutputStyle::Json = output_style {
            println!(
                "{}",
                serde_json::json!({ "orphans": orphans, "pruned": false })
            );
        }
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    if !skip_confirmation && !confirm_prune() {
        println!("Not pruning.");
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    match client.prune_orphans(&orphans).await {
        Ok(()) => match output_style {
            OutputStyle::Plain => println!("Pruned."),
            OutputStyle::Json => println!(
                "{}",
                serde_json::json!({ "orphans": orphans, "pruned": true })
            ),
        },
        Err(err) => {
            let message = format!("Could not prune orphans: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    guard.finish_with_outcome(log::Outcome::Ok);

    ExitCode::from(0)
}

#[cfg(test)]
mod crash_log_tests {
    use super::open_daemon_crash_log;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "douglas-crash-log-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        dir
    }

    #[test]
    fn test_open_daemon_crash_log_should_create_a_missing_parent_directory() {
        let dir = unique_temp_dir();
        let mut path = dir.clone();
        path.push("nested");
        path.push("resin.crash.log");
        assert!(!dir.exists());

        let result = open_daemon_crash_log(&path);

        assert!(result.is_some());
        assert!(path.exists());

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }

    #[test]
    fn test_open_daemon_crash_log_should_open_the_same_file_for_stdout_and_stderr() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let dir = unique_temp_dir();
        let Ok(()) = std::fs::create_dir_all(&dir) else {
            panic!("setup should succeed");
        };
        let mut path = dir.clone();
        path.push("resin.crash.log");

        let Some((mut stdout, mut stderr)) = open_daemon_crash_log(&path) else {
            panic!("should open crash log");
        };

        assert_ne!(stdout.as_raw_fd(), stderr.as_raw_fd());
        let Ok(()) = stdout.write_all(b"hello ") else {
            panic!("write should succeed");
        };
        let Ok(()) = stderr.write_all(b"world") else {
            panic!("write should succeed");
        };

        let mut contents = String::new();
        let Ok(mut file) = std::fs::File::open(&path) else {
            panic!("file should exist");
        };
        let Ok(_) = file.read_to_string(&mut contents) else {
            panic!("read should succeed");
        };
        assert_eq!(contents, "hello world");

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }

    #[test]
    fn test_open_daemon_crash_log_should_append_across_repeated_opens() {
        let dir = unique_temp_dir();
        let Ok(()) = std::fs::create_dir_all(&dir) else {
            panic!("setup should succeed");
        };
        let mut path = dir.clone();
        path.push("resin.crash.log");

        {
            use std::io::Write;
            let Some((mut stdout, _stderr)) = open_daemon_crash_log(&path) else {
                panic!("should open crash log");
            };
            let Ok(()) = stdout.write_all(b"first boot\n") else {
                panic!("write should succeed");
            };
        }
        {
            use std::io::Write;
            let Some((mut stdout, _stderr)) = open_daemon_crash_log(&path) else {
                panic!("should open crash log");
            };
            let Ok(()) = stdout.write_all(b"second boot\n") else {
                panic!("write should succeed");
            };
        }

        let Ok(contents) = std::fs::read_to_string(&path) else {
            panic!("read should succeed");
        };
        assert_eq!(contents, "first boot\nsecond boot\n");

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }
}

// Keeps testing-utils/smoke-tests/steps/*.sh honest: each step's `# covers:`
// line is the shared source of truth between this test and the NixOS smoke
// test suite, so a CLI command added/renamed/removed here without updating
// a step to match fails fast in `cargo test` instead of silently rotting the
// smoke test coverage claim.
#[cfg(test)]
mod command_coverage_tests {
    use super::Cli;
    use clap::CommandFactory;
    use std::path::PathBuf;

    fn collect_leaf_command_paths(command: &clap::Command, prefix: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for sub in command.get_subcommands() {
            if sub.is_hide_set() {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.has_subcommands() {
                paths.extend(collect_leaf_command_paths(sub, &path));
            } else {
                paths.push(path);
            }
        }
        paths
    }

    fn steps_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testing-utils/smoke-tests/steps")
    }

    fn covered_command_paths() -> Vec<String> {
        let dir = steps_dir();
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("should read {}: {err}", dir.display()));

        let mut covered = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                panic!("should read directory entry");
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("sh") {
                continue;
            }
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("should read {}: {err}", path.display()));
            for line in contents.lines() {
                if let Some(command) = line.trim().strip_prefix("# covers:") {
                    covered.push(command.trim().to_string());
                }
            }
        }
        covered
    }

    #[test]
    fn test_smoke_test_steps_should_cover_every_non_hidden_cli_command() {
        let mut actual = collect_leaf_command_paths(&Cli::command(), "");
        actual.sort();
        actual.dedup();

        let mut expected = covered_command_paths();
        expected.sort();
        expected.dedup();

        assert_eq!(
            actual, expected,
            "testing-utils/smoke-tests/steps/*.sh is out of sync with the CLI's actual command \
             surface (left = live CLI commands, right = commands claimed via `# covers:` \
             lines) — add, rename, or remove a step's `# covers:` line to match"
        );
    }
}
