use crate::daemon::{build_cli_reporter, initgroups_for, open_daemon_crash_log, run_with_tokio};
use ::config::DouglasFolders;
use daemonize::Daemonize;
use file_system::{
    FileDeleter, FileReader, FileWriter, UnixFileDeleter, UnixFileReader, UnixFileWriter,
};
use log::{BufferedFileReporter, Reporter};
use os::{Os, Unix};
use std::{process::ExitCode, sync::Arc};

pub(crate) fn start_bract(reporting_fd: i32) -> ExitCode {
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

pub(crate) fn start_resin(reporting_fd: i32) -> ExitCode {
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

pub(crate) async fn resin_debug_mode() -> ExitCode {
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

pub(crate) fn start_seedbank(reporting_fd: i32) -> ExitCode {
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

pub(crate) async fn seedbank_debug_mode() -> ExitCode {
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

pub(crate) fn start_woodward() -> ExitCode {
    let mut daemonize = Daemonize::new()
        .user(woodward::DOUGLAS_WOODWARD_USER)
        .group(woodward::DOUGLAS_WOODWARD_GROUP)
        .privileged_action(|| initgroups_for(woodward::DOUGLAS_WOODWARD_USER));
    let crash_log_path = DouglasFolders::new()
        .log_dir(woodward::WOODWARD)
        .join(format!("{}.crash.log", woodward::WOODWARD));
    if let Some((stdout, stderr)) = open_daemon_crash_log(&crash_log_path) {
        daemonize = daemonize.stdout(stdout).stderr(stderr);
    }

    match daemonize.start() {
        Ok(()) => run_with_tokio(run_woodward_server()),
        Err(err) => {
            eprintln!("Failed to daemonize seedbank server: {err:?}");
            ExitCode::from(1)
        }
    }
}

async fn run_woodward_server() -> ExitCode {
    let douglas_folders = DouglasFolders::new();
    let reporter: Arc<dyn Reporter> = Arc::new(BufferedFileReporter::new(
        douglas_folders.service_log_file(woodward::WOODWARD),
    ));
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
    let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());

    let server = Arc::new(woodward::Server::new(
        reporter,
        file_reader,
        file_writer,
        file_deleter,
        os,
        douglas_folders,
    ));

    if let Err(err) = server.start().await {
        eprintln!("Failed to start woodward: {err:?}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}

pub(crate) async fn woodward_debug_mode() -> ExitCode {
    let douglas_folders = DouglasFolders::new();
    let reporter: Arc<dyn Reporter> =
        if let Ok(reporter) = build_cli_reporter(&douglas_folders, woodward::WOODWARD) {
            reporter
        } else {
            eprintln!("Failed to start TUI reporter");
            return ExitCode::from(1);
        };
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
    let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
    let os: Arc<dyn Os> = Arc::new(Unix::new());

    let server = Arc::new(woodward::Server::new(
        reporter,
        file_reader,
        file_writer,
        file_deleter,
        os,
        douglas_folders,
    ));

    match server.start().await {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("woodward: {err}");
            ExitCode::from(1)
        }
    }
}
