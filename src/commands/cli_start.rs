use crate::commands::cli_start::CliBootstrapError::SpawnError;
use blueprint::{Command, CommandExecutor, JournalingExecutor, RunningStatus};
use command_fds::{CommandFdExt, FdMapping, FdMappingCollision};
use config::DouglasFolders;
use credentials::{Credentials, CredentialsError};
use file_system::{FileReader, FileSystemError};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{Os, OsError};
use os_pipe::{PipeReader, PipeWriter};
use std::os::{fd::OwnedFd, unix::io::AsRawFd};
use std::{
    num::ParseIntError,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliBootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Pipe required")]
    PipeRequired,
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Invalid PID: {0}")]
    InvalidPid(#[from] ParseIntError),
    #[error("Could not determine pid liveliness {0}")]
    PidLiveliness(#[from] OsError),
    #[error("Pipe error: {0}")]
    PipeError(#[from] std::io::Error),
    #[error("Spawn error: {0}")]
    SpawnError(#[from] FdMappingCollision),
    #[error("Timed out waiting for bract to start (5 minutes exceeded)")]
    BractStartTimeout,
}

type Step = Box<dyn Command<Context, Error = CliBootstrapError>>;

fn push_step(
    steps: &mut Vec<Step>,
    command: impl Command<Context, Error = CliBootstrapError> + 'static,
) {
    steps.push(Box::new(command));
}

struct Context {
    os: Arc<dyn Os>,
    pipe_reader: Option<PipeReader>,
    pipe_writer: Option<PipeWriter>,
}

#[derive(Default)]
struct State {
    is_root: bool,
    is_bract_running: RunningStatus,
    //TODO: core apps installed
}

struct StateObserver {
    credentials: Arc<dyn Credentials>,
    file_reader: Arc<dyn FileReader>,
    os: Arc<dyn Os>,
}

impl StateObserver {
    pub fn new(
        credentials: Arc<dyn Credentials>,
        file_reader: Arc<dyn FileReader>,
        os: Arc<dyn Os>,
    ) -> Self {
        Self {
            credentials,
            file_reader,
            os,
        }
    }

    pub fn discover(
        &self,
        span: &Span,
        config: &DouglasFolders,
    ) -> Result<State, CliBootstrapError> {
        let guard = span
            .create_child("Discovering current state", ScopeKind::Phase)
            .start_guard();
        let mut result = State {
            is_root: self.credentials.is_root(),
            ..Default::default()
        };
        result.is_bract_running = match self.get_bract_pid(&config.transients)? {
            Some(pid) => {
                if self.os.is_active_pid(pid)? {
                    RunningStatus::Running
                } else {
                    RunningStatus::NotRunning
                }
            }
            None => RunningStatus::NotRunning,
        };

        guard.finish(Ok(result))
    }

    fn get_bract_pid(&self, sockets_root: &Path) -> Result<Option<u32>, CliBootstrapError> {
        let mut expected_path = sockets_root.to_path_buf();
        expected_path.push("bract.pid");

        if !self.file_reader.exists(&expected_path) {
            return Ok(None);
        }

        let raw_pid = self.file_reader.read_all(&expected_path)?;
        let pid: u32 = raw_pid.trim().parse()?;

        Ok(Some(pid))
    }
}

fn create_plan(state: &State) -> Result<Vec<Step>, CliBootstrapError> {
    let mut result = Vec::new();
    if state.is_bract_running == RunningStatus::Running {
        return Ok(result);
    }

    if !state.is_root {
        return Err(CliBootstrapError::MustBeRoot);
    }

    push_step(&mut result, CreatePipe::new());
    push_step(&mut result, StartBract::new());

    Ok(result)
}

struct CreatePipe {}

impl CreatePipe {
    pub fn new() -> Self {
        Self {}
    }
}

impl std::fmt::Display for CreatePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Create pipe")
    }
}

impl Command<Context> for CreatePipe {
    type Error = CliBootstrapError;

    fn name(&self) -> String {
        "Create Pipe".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
        let guard = span
            .create_child("Creating pipe…", ScopeKind::Step)
            .start_guard();

        // os_pipe sets O_CLOEXEC by default — child won't inherit unless we explicitly clear it.
        let (read_fd, write_fd) = os_pipe::pipe()?;
        context.pipe_reader = Some(read_fd);
        context.pipe_writer = Some(write_fd);

        guard.finish(Ok(()))
    }
}

struct StartBract {}

impl StartBract {
    pub fn new() -> Self {
        Self {}
    }
}

impl std::fmt::Display for StartBract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Start bract")
    }
}

impl Command<Context> for StartBract {
    type Error = CliBootstrapError;

    fn name(&self) -> String {
        "Start bract".to_string()
    }

    fn run(&mut self, span: &Span, context: &mut Context) -> Result<(), Self::Error> {
        let guard = span
            .create_child("Starting bract…", ScopeKind::Step)
            .start_guard();

        let Some(pipe_writer) = context.pipe_writer.take() else {
            return Err(CliBootstrapError::PipeRequired);
        };
        let Some(pipe_reader) = context.pipe_reader.take() else {
            return Err(CliBootstrapError::PipeRequired);
        };

        let fd = pipe_writer.as_raw_fd();
        match std::process::Command::new(context.os.current_executable()?)
            .args(["start", "--bract", "--notify-fd", &fd.to_string()])
            .fd_mappings(vec![FdMapping {
                parent_fd: OwnedFd::from(pipe_writer),
                child_fd: fd,
            }]) {
            Ok(mut cmd) => cmd.spawn()?,
            Err(err) => return guard.finish(Err(SpawnError(err))),
        };

        /*
         * Read bract's event stream on a background thread
         * forward events to the reporter so they appear in the ui
         * Bail with a five minute timeout.
         */
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(pipe_reader);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
            // Sender drops here — receiver will see Disconnected when pipe closes.
        });

        let deadline = Instant::now() + Duration::from_secs(5 * 60);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return guard.finish(Err(CliBootstrapError::BractStartTimeout));
            }
            match rx.recv_timeout(remaining) {
                Ok(line) => {
                    let Ok(event) = serde_json::from_str::<log::Event>(&line) else {
                        continue;
                    };
                    span.reporter.emit(event);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // pipe closed — bract ready
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return guard.finish(Err(CliBootstrapError::BractStartTimeout));
                }
            }
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub async fn cli_start(
    reporter: Arc<dyn Reporter>,
    plan_only: bool,
    credentials: Arc<dyn Credentials>,
    file_reader: Arc<dyn FileReader>,
    os: Arc<dyn Os>,
    douglas_folders: DouglasFolders,
) {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();
    let state_observer = StateObserver::new(Arc::clone(&credentials), file_reader, Arc::clone(&os));
    let state = match state_observer.discover(guard.span(), &douglas_folders) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    let plan = match create_plan(&state) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    guard
        .span()
        .plan_hint(plan.iter().map(std::string::ToString::to_string).collect());

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return;
    }

    let mut executor = JournalingExecutor::new();
    let mut context = Context {
        os,
        pipe_reader: None,
        pipe_writer: None,
    };

    match executor.run(
        &guard
            .span()
            .create_child("Executing plan", ScopeKind::Phase),
        &mut context,
        plan,
    ) {
        blueprint::ExecutionResult::Success => {
            guard.finish_with_outcome(Outcome::Ok);
        }
        blueprint::ExecutionResult::Failed {
            failed_at_step,
            failed_at_step_name,
            perform_error,
            rollback_errors,
        } => {
            guard.span().message(
                Level::Warn,
                &format!(
                    "Failed at step {failed_at_step}. {failed_at_step_name}: '{perform_error}'"
                ),
            );
            if !rollback_errors.is_empty() {
                guard.span().message(
                    Level::Warn,
                    "Additionally, ran into these errors while rolling back",
                );
                for error in rollback_errors {
                    guard.span().message(Level::Warn, &error.to_string());
                }
            }
            guard.finish_with_outcome(Outcome::Failed);
        }
    }
}
