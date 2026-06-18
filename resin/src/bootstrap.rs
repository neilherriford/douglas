use crate::Error;
use blueprint::{Command, CommandExecutor, HasFolder, JournalingExecutor, commands::CreateFolder};
use credentials::Credentials;
use file_system::Folder;
use log::{BufferedFileReporter, Level, PipeReporter, Reporter, ScopeKind, Span, TeeReporter};
use std::{path::PathBuf, sync::Arc};

pub async fn bootstrap(
    reporting_fd: i32,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    log_path: PathBuf,
    root_path: PathBuf,
    blobs_path: PathBuf,
    repositories_path: PathBuf,
) -> Result<(), Error> {
    let boot_reporter: Arc<dyn Reporter> = {
        let mut sinks: Vec<Box<dyn Reporter>> = vec![Box::new(BufferedFileReporter::new(log_path))];

        sinks.push(Box::new(unsafe { PipeReporter::from_raw_fd(reporting_fd) }));
        Arc::new(TeeReporter::new(sinks))
    };

    let guard = Span::new(
        Arc::clone(&boot_reporter),
        "Bootstrapping douglas-resin system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver::new(credentials, folder);
        state_observer
            .discover(guard.span(), root_path, blobs_path, repositories_path)
            .await?
    };

    let plan = match create_plan(state) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return guard.finish(Err(err));
        }
    };

    guard
        .span()
        .plan_hint(plan.iter().map(std::string::ToString::to_string).collect());

    let mut executor = JournalingExecutor::new();
    let execution_result = {
        let mut context = Context { folder };
        executor.run(
            &guard
                .span()
                .create_child("Executing plan", ScopeKind::Phase),
            &mut context,
            plan,
        )
    };

    match execution_result {
        blueprint::ExecutionResult::Success => guard.finish(Ok(())),
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
            guard.finish(Err(Error::FailedBoostrap(Vec::new())))
        }
    }
}

#[derive(Default)]
struct State {
    is_root: bool,
    root_path_exists: bool,
    folders_missing: Vec<PathBuf>,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
}

impl<'a> StateObserver<'a> {
    pub fn new(credentials: &'a dyn Credentials, folder: &'a dyn Folder) -> Self {
        Self {
            credentials,
            folder,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        root_path: PathBuf,
        blobs_path: PathBuf,
        repositories_path: PathBuf,
    ) -> Result<State, Error> {
        let guard = span
            .create_child(
                "Starting resin system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State {
            is_root: self.credentials.is_root(),
            root_path_exists: self.folder.exists(&root_path),
            ..Default::default()
        };

        if result.is_root || !result.root_path_exists {
            return guard.finish(Ok(result));
        }

        self.check_folder_presence(&mut result, blobs_path);
        self.check_folder_presence(&mut result, repositories_path);

        guard.finish(Ok(result))
    }

    fn check_folder_presence(&mut self, result: &mut State, path: PathBuf) {
        if !self.folder.exists(&path) {
            result.folders_missing.push(path);
        }
    }
}

struct Context<'a> {
    folder: &'a dyn Folder,
}

impl<'a> HasFolder for Context<'a> {
    fn folder(&self) -> &dyn Folder {
        self.folder
    }
}

type Step = Box<dyn for<'a> Command<Context<'a>>>;

fn push_step(steps: &mut Vec<Step>, command: impl for<'a> Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

fn create_plan(state: State) -> Result<Vec<Step>, Error> {
    let mut result = Vec::new();

    if state.is_root {
        return Err(Error::CannotBeRoot);
    }

    if !state.root_path_exists {
        return Err(Error::MissingRootPath);
    }

    for folder in state.folders_missing {
        push_step(&mut result, CreateFolder::new(folder.clone()));
    }

    Ok(result)
}
