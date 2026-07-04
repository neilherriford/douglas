use blueprint::{
    Command, GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, RunningStatus,
    bootstrap::{execute_plan, resolve_plan},
    commands::{AddUserToGroup, CreateGroup},
    listener::{LivenessCheck, check_liveness},
    service::BootstrapReporting,
};
use command_fds::{CommandFdExt, FdMapping, FdMappingCollision};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
use file_system::{Folder, Permissions};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{EnvironmentVariableReader, Os};
use os_pipe::{PipeReader, PipeWriter};
use std::{
    collections::HashMap,
    env::VarError,
    os::{fd::OwnedFd, unix::io::AsRawFd},
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliBootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Pipe required")]
    PipeRequired,
    #[error("Spawn error: {0}")]
    SpawnError(#[from] FdMappingCollision),
    #[error("Timed out waiting for {0} to start (5 minutes exceeded)")]
    StartTimeout(String),
    #[error("Service '{0}' has no configured control socket")]
    MissingControlSocket(&'static str),
}

type Step = Box<dyn for<'a> Command<Context<'a>>>;

fn push_step(steps: &mut Vec<Step>, command: impl for<'a> Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct DouglasService {
    name: &'static str,
    bootstrap_reporting: BootstrapReporting,
    liveness: LivenessCheck,
}

fn known_services(
    douglas_folders: &DouglasFolders,
) -> Result<Vec<DouglasService>, CliBootstrapError> {
    let bract_definition = bract::service_definition(douglas_folders);
    let Some(bract_socket) = bract_definition.owned_sockets.into_iter().next() else {
        return Err(CliBootstrapError::MissingControlSocket("bract"));
    };

    Ok(vec![
        DouglasService {
            name: "bract",
            bootstrap_reporting: bract_definition.bootstrap_reporting,
            liveness: LivenessCheck::UnixSocket(bract_socket.socket_path),
        },
        DouglasService {
            name: "resin",
            bootstrap_reporting: resin::service_definition(douglas_folders).bootstrap_reporting,
            liveness: LivenessCheck::TcpPort {
                host: "127.0.0.1".to_string(),
                port: resin::DEFAULT_PORT,
            },
        },
    ])
}

struct Context<'a> {
    os: &'a dyn Os,
    credentials: &'a dyn Credentials,
    permissions: &'a dyn Permissions,
    folder: &'a dyn Folder,
    pipes: HashMap<String, (PipeReader, PipeWriter)>,
}

impl HasCredentials for Context<'_> {
    fn credentials(&self) -> &dyn Credentials {
        self.credentials
    }
}

impl HasFolder for Context<'_> {
    fn folder(&self) -> &dyn Folder {
        self.folder
    }
}

impl HasPermissions for Context<'_> {
    fn permissions(&self) -> &dyn Permissions {
        self.permissions
    }
}

#[derive(Default)]
struct State {
    is_root: bool,
    groups_missing: Vec<String>,
    group_members_missing: Vec<GroupMembershipRequirement>,
    services_needing_start: Vec<DouglasService>,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
    folder: &'a dyn Folder,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
        folder: &'a dyn Folder,
    ) -> Self {
        Self {
            credentials,
            environment_variable_reader,
            folder,
        }
    }

    pub fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, CliBootstrapError> {
        let guard = span
            .create_child(
                "Starting douglas system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        if !self.credentials.is_root() {
            return guard.finish(Ok(State::default()));
        }

        let mut result = State {
            is_root: true,
            ..Default::default()
        };

        self.check_admin_group_membership(guard.span(), &mut result);

        let services = match known_services(douglas_folders) {
            Ok(services) => services,
            Err(err) => return guard.finish(Err(err)),
        };

        for service in services {
            let status = check_liveness(guard.span(), self.folder, &service.liveness);
            if status != RunningStatus::Running {
                result.services_needing_start.push(service);
            }
        }

        guard.finish(Ok(result))
    }

    fn check_admin_group_membership(&mut self, span: &Span, result: &mut State) {
        let (non_sudoer, valid_non_sudoer) = self.get_non_sudoer(span);
        if self.credentials.group_exists(DOUGLAS_ADMIN_GROUP) {
            if valid_non_sudoer
                && !self
                    .credentials
                    .group_memberships(DOUGLAS_ADMIN_GROUP)
                    .contains(&non_sudoer)
            {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement::new(
                        DOUGLAS_ADMIN_GROUP,
                        &non_sudoer,
                    ));
            }
        } else {
            result.groups_missing.push(DOUGLAS_ADMIN_GROUP.to_string());
            if valid_non_sudoer {
                result
                    .group_members_missing
                    .push(GroupMembershipRequirement::new(
                        DOUGLAS_ADMIN_GROUP,
                        &non_sudoer,
                    ));
            }
        }
    }

    fn get_non_sudoer(&self, span: &Span) -> (String, bool) {
        match self.environment_variable_reader.read("SUDO_USER") {
            Ok(user_name) => {
                let valid = user_name != credentials::ROOT_USER_NAME;
                (user_name, valid)
            }
            Err(VarError::NotPresent) => (credentials::ROOT_USER_NAME.to_string(), false),
            Err(VarError::NotUnicode(_)) => {
                span.message(Level::Warn, &format!(
                            "Could not determine initiating user?  You will need to manually add the \
                                account you wish to interact with the Douglas CLI to the '{DOUGLAS_ADMIN_GROUP}' \
                                manually!"
                        ));
                (credentials::ROOT_USER_NAME.to_string(), false)
            }
        }
    }
}

fn create_plan(state: State) -> Result<Vec<Step>, CliBootstrapError> {
    if !state.is_root {
        return Err(CliBootstrapError::MustBeRoot);
    }

    let mut result = Vec::new();

    for group_name in &state.groups_missing {
        push_step(&mut result, CreateGroup::new(group_name));
    }

    for membership in &state.group_members_missing {
        push_step(
            &mut result,
            AddUserToGroup::new(&membership.user_name, &membership.group_name),
        );
    }

    for service in state.services_needing_start {
        if matches!(service.bootstrap_reporting, BootstrapReporting::Pipe) {
            push_step(&mut result, CreatePipe::new(service.name));
        }
        push_step(&mut result, StartService::new(service));
    }

    Ok(result)
}

struct CreatePipe {
    service_name: String,
}

impl CreatePipe {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
        }
    }
}

impl std::fmt::Display for CreatePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create pipe for {}", self.service_name)
    }
}

impl<'a> Command<Context<'a>> for CreatePipe {
    fn name(&self) -> String {
        "Create Pipe".to_string()
    }

    fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child("Creating pipe…", ScopeKind::Step)
            .start_guard();

        let (reader, writer) = os_pipe::pipe()?;
        context
            .pipes
            .insert(self.service_name.clone(), (reader, writer));

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct StartService {
    process: DouglasService,
}

impl StartService {
    pub fn new(process: DouglasService) -> Self {
        Self { process }
    }
}

impl std::fmt::Display for StartService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Start {}", self.process.name)
    }
}

impl<'a> Command<Context<'a>> for StartService {
    fn name(&self) -> String {
        format!("Start {}", self.process.name)
    }

    fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child(&format!("Starting {}…", self.process.name), ScopeKind::Step)
            .start_guard();

        let pipe = if matches!(self.process.bootstrap_reporting, BootstrapReporting::Pipe) {
            let Some(pipe) = context.pipes.remove(self.process.name) else {
                return Err(Box::new(CliBootstrapError::PipeRequired));
            };
            Some(pipe)
        } else {
            None
        };

        spawn_service(
            &self.process,
            pipe,
            context.os,
            context.folder,
            guard.span(),
        )?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

fn spawn_service(
    process: &DouglasService,
    pipe: Option<(PipeReader, PipeWriter)>,
    os: &dyn Os,
    folder: &dyn Folder,
    span: &Span,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new(os.current_executable()?);
    command.args(["service", process.name]);

    match pipe {
        Some((pipe_reader, pipe_writer)) => {
            let fd = pipe_writer.as_raw_fd();
            command.args(["--notify-fd", &fd.to_string()]);
            match command.fd_mappings(vec![FdMapping {
                parent_fd: OwnedFd::from(pipe_writer),
                child_fd: fd,
            }]) {
                Ok(cmd) => {
                    cmd.spawn()?;
                }
                Err(err) => return Err(Box::new(CliBootstrapError::SpawnError(err))),
            }
            forward_logs_in_background(pipe_reader, span.clone());
        }
        None => {
            command.spawn()?;
        }
    }

    wait_until_running(process, folder, span)
}

fn forward_logs_in_background(pipe_reader: PipeReader, span: Span) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(pipe_reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(event) = serde_json::from_str::<log::Event>(&line) else {
                continue;
            };
            span.reporter.emit(event);
        }
    });
}

fn wait_until_running(
    process: &DouglasService,
    folder: &dyn Folder,
    span: &Span,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_mins(5);
    loop {
        if check_liveness(span, folder, &process.liveness) == RunningStatus::Running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Box::new(CliBootstrapError::StartTimeout(
                process.name.to_string(),
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub async fn cli_start(
    reporter: Arc<dyn Reporter>,
    plan_only: bool,
    credentials: Arc<dyn Credentials>,
    permissions: Arc<dyn Permissions>,
    environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    folder: Arc<dyn Folder>,
    os: Arc<dyn Os>,
    douglas_folders: DouglasFolders,
) {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Starting douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut state_observer = StateObserver::new(
        credentials.as_ref(),
        environment_variable_reader.as_ref(),
        folder.as_ref(),
    );
    let state = match state_observer.discover(guard.span(), &douglas_folders) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    let plan = match resolve_plan(guard.span(), create_plan(state)) {
        Ok(plan) => plan,
        Err(_) => {
            guard.finish_with_outcome(log::Outcome::Failed);
            return;
        }
    };

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return;
    }

    let mut context = Context {
        os: os.as_ref(),
        credentials: credentials.as_ref(),
        folder: folder.as_ref(),
        permissions: permissions.as_ref(),
        pipes: HashMap::new(),
    };

    match execute_plan(guard.span(), plan, &mut context, || ()) {
        Ok(()) => guard.finish_with_outcome(Outcome::Ok),
        Err(()) => {
            guard.finish_with_outcome(Outcome::Failed);
        }
    }
}
