use crate::Error;
use blueprint::{
    Command,
    bootstrap::{build_boot_reporter, execute_plan, resolve_plan},
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser, discover_service_state,
        plan_service_bootstrap,
    },
};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_RESIN_SEEDBANK_GROUP};
use file_system::{Folder, Modes, Permissions};
use log::{ScopeKind, Span};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub static DOUGLAS_RESIN_USER: &str = "douglas-resin";
pub static DOUGLAS_RESIN_GROUP: &str = "douglas-resin";
pub static RESIN: &str = "resin";

pub async fn bootstrap(
    reporting_fd: Option<i32>,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    log_path: PathBuf,
    root_path: PathBuf,
    repositories_path: PathBuf,
) -> Result<(), Error> {
    let boot_reporter = build_boot_reporter(log_path, reporting_fd);

    let guard = Span::new(
        Arc::clone(&boot_reporter),
        "Bootstrapping douglas-resin system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let definition = definition_for(repositories_path);

    let state = {
        let mut state_observer = StateObserver::new(credentials, folder);
        state_observer
            .discover(
                guard.span(),
                &definition,
                &root_path,
                credentials,
                folder,
                permissions,
            )
            .await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(&definition, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut context = Context::new(credentials, folder, permissions);
    let result = execute_plan(guard.span(), plan, &mut context, || {
        Error::FailedBoostrap(Vec::new())
    });

    guard.finish(result)
}

pub fn service_definition(douglas_folders: &DouglasFolders) -> ServiceDefinition {
    let mut repositories_path = douglas_folders.service_root(RESIN);
    repositories_path.push("repositories");
    definition_for(repositories_path)
}

fn definition_for(repositories_path: PathBuf) -> ServiceDefinition {
    ServiceDefinition::with_sockets(
        ServiceUser::create_managed(DOUGLAS_RESIN_USER),
        DOUGLAS_RESIN_GROUP,
        vec![(
            repositories_path,
            Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
        )],
        Vec::new(),
        &[DOUGLAS_RESIN_SEEDBANK_GROUP],
        BootstrapReporting::Pipe,
    )
}

#[derive(Default)]
struct State {
    is_root: bool,
    root_path_exists: bool,
    service: ServiceState,
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
        definition: &ServiceDefinition,
        root_path: &Path,
        credentials: &dyn Credentials,
        folder: &dyn Folder,
        permissions: &dyn Permissions,
    ) -> Result<State, Error> {
        let guard = span
            .create_child(
                "Starting resin system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let root_path_exists = self.folder.exists(root_path);

        let mut result = State {
            is_root: self.credentials.is_root(),
            root_path_exists,
            ..Default::default()
        };

        if result.is_root || !result.root_path_exists {
            return guard.finish(Ok(result));
        }

        result.service = discover_service_state(definition, credentials, folder, permissions)?;

        guard.finish(Ok(result))
    }
}

type Context<'a> = blueprint::StandardContext<'a>;
type Step<'a> = Box<dyn Command<Context<'a>>>;

fn create_plan<'a>(definition: &ServiceDefinition, state: State) -> Result<Vec<Step<'a>>, Error> {
    if state.is_root {
        return Err(Error::CannotBeRoot);
    }

    if !state.root_path_exists {
        return Err(Error::MissingRootPath);
    }

    Ok(plan_service_bootstrap(definition, &state.service))
}
