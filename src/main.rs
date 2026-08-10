mod bootstrap;
mod config;
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
use file_system::{Folder, Permissions, UnixFolder, UnixPermissions};
use log::{BufferedFileReporter, Event, Reporter, Span, TeeReporter};
use os::{EnvironmentVariableReader, Os, Unix, UnixEnvironmentVariableReader};
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
    #[command(about = "Create a blank template for a seedling")]
    CreateTemplate,
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
                seedling: SeedlingCommand::CreateTemplate,
            } => f.write_str("create seedling template"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output_style = cli.output_style;

    match cli.command {
        Commands::Start { plan_only } => run_with_tokio(start(plan_only)),
        Commands::Stop => todo!(),
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
            seedling: SeedlingCommand::CreateTemplate,
        } => create_seedling_template(output_style),
    }
}

fn example_seedling_spec() -> seedbank_types::SeedlingSpec {
    let mut mounts = HashMap::new();

    mounts.insert(
        "config".parse().expect("valid name"),
        seedbank_types::Mount::build(
            seedbank_types::MountType::Persisted,
            PathBuf::from("/etc/example/config"),
            seedbank_types::AccessMode::ReadOnly,
            HashSet::new(),
        ),
    );

    mounts.insert(
        "cache".parse().expect("valid name"),
        seedbank_types::Mount::build(
            seedbank_types::MountType::InMemory,
            PathBuf::from("/var/cache/example"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    mounts.insert(
        "shared-assets".parse().expect("valid name"),
        seedbank_types::Mount::build(
            seedbank_types::MountType::PersistedShared(vec![
                "sibling-a".parse().expect("valid name"),
                "sibling-b".parse().expect("valid name"),
            ]),
            PathBuf::from("/var/lib/example/shared"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    seedbank_types::SeedlingSpec::new(mounts)
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

// Daemonize redirects stdout/stderr to `/dev/null` by default, lets
// keep logs for crashes that happen pre-fork
fn open_daemon_crash_log(path: &Path) -> Option<(std::fs::File, std::fs::File)> {
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

async fn start(plan_only: bool) -> ExitCode {
    let douglas_folders = DouglasFolders::new();
    let Ok(reporter) = build_cli_reporter(&douglas_folders, "douglas-cli") else {
        eprintln!("Failed to start TUI reporter");
        return ExitCode::from(1);
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

    let succeeded = bootstrap::seedlings::perform(reporter, bract_client).await;

    if !succeeded {
        return ExitCode::from(1);
    }

    ExitCode::from(0)
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
