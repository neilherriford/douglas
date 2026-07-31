use blueprint::{
    Command, GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, RunningStatus,
    bootstrap::{execute_plan, resolve_plan},
    commands::{AddUserToGroup, CreateGroup},
    listener::{LivenessCheck, check_liveness},
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, discover_service_state,
        plan_service_bootstrap,
    },
};
use async_trait::async_trait;
use command_fds::{CommandFdExt, FdMapping, FdMappingCollision};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
use file_system::{FileSystemError, Folder, Permissions};
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
pub enum BootstrapError {
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
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct DouglasService {
    name: &'static str,
    bootstrap_reporting: BootstrapReporting,
    liveness: LivenessCheck,
    definition: ServiceDefinition,
}

fn known_services(douglas_folders: &DouglasFolders) -> Result<Vec<DouglasService>, BootstrapError> {
    let bract_definition = bract::service_definition(douglas_folders);
    let Some(bract_socket) = bract_definition.owned_sockets.first() else {
        return Err(BootstrapError::MissingControlSocket("bract"));
    };
    let bract_liveness = LivenessCheck::UnixSocket(bract_socket.socket_path.clone());

    let seedbank_definition = seedbank::service_definition(douglas_folders);
    let Some(seedbank_socket) = seedbank_definition.owned_sockets.first() else {
        return Err(BootstrapError::MissingControlSocket(seedbank::SEEDBANK));
    };
    let seedbank_liveness = LivenessCheck::UnixSocket(seedbank_socket.socket_path.clone());

    let resin_definition = resin::service_definition(douglas_folders);

    Ok(vec![
        DouglasService {
            name: "bract",
            bootstrap_reporting: bract_definition.bootstrap_reporting,
            liveness: bract_liveness,
            definition: bract_definition,
        },
        DouglasService {
            name: resin::RESIN,
            bootstrap_reporting: resin_definition.bootstrap_reporting,
            liveness: LivenessCheck::TcpPort {
                host: "127.0.0.1".to_string(),
                port: resin_types::DEFAULT_PORT,
            },
            definition: resin_definition,
        },
        DouglasService {
            name: seedbank::SEEDBANK,
            bootstrap_reporting: seedbank_definition.bootstrap_reporting,
            liveness: seedbank_liveness,
            definition: seedbank_definition,
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
    services_needing_start: Vec<(DouglasService, ServiceState)>,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
    folder: &'a dyn Folder,
    permissions: &'a dyn Permissions,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
        folder: &'a dyn Folder,
        permissions: &'a dyn Permissions,
    ) -> Self {
        Self {
            credentials,
            environment_variable_reader,
            folder,
            permissions,
        }
    }

    pub fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, BootstrapError> {
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
            let status = check_liveness(guard.span(), &service.liveness);
            if status != RunningStatus::Running {
                let service_state = match discover_service_state(
                    &service.definition,
                    self.credentials,
                    self.folder,
                    self.permissions,
                ) {
                    Ok(service_state) => service_state,
                    Err(err) => return guard.finish(Err(err.into())),
                };
                result.services_needing_start.push((service, service_state));
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

fn create_plan<'a>(state: State) -> Result<Vec<Step<'a>>, BootstrapError> {
    if !state.is_root {
        return Err(BootstrapError::MustBeRoot);
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

    for (service, service_state) in &state.services_needing_start {
        for step in plan_service_bootstrap::<Context<'a>>(&service.definition, service_state) {
            result.push(step);
        }
    }

    for (service, _) in &state.services_needing_start {
        if matches!(service.bootstrap_reporting, BootstrapReporting::Pipe) {
            push_step(&mut result, CreatePipe::new(service.name));
        }
    }

    for (service, _) in &state.services_needing_start {
        let needs_reporting_pipe = matches!(service.bootstrap_reporting, BootstrapReporting::Pipe);
        push_step(
            &mut result,
            StartService::new(service.name, needs_reporting_pipe),
        );
    }

    for (service, _) in state.services_needing_start {
        push_step(
            &mut result,
            WaitForServiceReady::new(service.name, service.liveness),
        );
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

#[async_trait(?Send)]
impl<'a> Command<Context<'a>> for CreatePipe {
    fn name(&self) -> String {
        "Create Pipe".to_string()
    }

    async fn run(
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
    name: &'static str,
    needs_reporting_pipe: bool,
}

impl StartService {
    pub fn new(name: &'static str, needs_reporting_pipe: bool) -> Self {
        Self {
            name,
            needs_reporting_pipe,
        }
    }
}

impl std::fmt::Display for StartService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Start {}", self.name)
    }
}

#[async_trait(?Send)]
impl<'a> Command<Context<'a>> for StartService {
    fn name(&self) -> String {
        format!("Start {}", self.name)
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child(&format!("Starting {}…", self.name), ScopeKind::Step)
            .start_guard();

        let pipe = if self.needs_reporting_pipe {
            let Some(pipe) = context.pipes.remove(self.name) else {
                return Err(Box::new(BootstrapError::PipeRequired));
            };
            Some(pipe)
        } else {
            None
        };

        spawn_service(self.name, pipe, context.os, guard.span())?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

struct WaitForServiceReady {
    name: &'static str,
    liveness: LivenessCheck,
}

impl WaitForServiceReady {
    pub fn new(name: &'static str, liveness: LivenessCheck) -> Self {
        Self { name, liveness }
    }
}

impl std::fmt::Display for WaitForServiceReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wait for {} to be ready", self.name)
    }
}

#[async_trait(?Send)]
impl<'a> Command<Context<'a>> for WaitForServiceReady {
    fn name(&self) -> String {
        format!("Wait for {}", self.name)
    }

    async fn run(
        &mut self,
        span: &Span,
        _context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child(&format!("Waiting for {}…", self.name), ScopeKind::Step)
            .start_guard();

        wait_until_running(self.name, &self.liveness, guard.span()).await?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

fn spawn_service(
    name: &'static str,
    pipe: Option<(PipeReader, PipeWriter)>,
    os: &dyn Os,
    span: &Span,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new(os.current_executable()?);
    command.args(["service", name]);

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
                Err(err) => return Err(Box::new(BootstrapError::SpawnError(err))),
            }
            forward_logs_in_background(name, pipe_reader, span.clone());
        }
        None => {
            command.spawn()?;
        }
    }

    Ok(())
}

fn forward_logs_in_background(service_name: &'static str, pipe_reader: PipeReader, span: Span) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(pipe_reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(mut event) = serde_json::from_str::<log::Event>(&line) else {
                continue;
            };
            tag_event_with_service(&mut event, service_name);
            span.reporter.emit(event);
        }
    });
}

fn tag_event_with_service(event: &mut log::Event, service_name: &str) {
    match &mut event.kind {
        log::EventKind::ScopeStarted { label, .. } | log::EventKind::ScopeEnded { label, .. } => {
            *label = format!("[{service_name}] {label}");
        }
        log::EventKind::Message { text, .. } => {
            *text = format!("[{service_name}] {text}");
        }
        log::EventKind::PlanHint { .. } | log::EventKind::Progress { .. } => {}
    }
}

async fn wait_until_running(
    name: &str,
    liveness: &LivenessCheck,
    span: &Span,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_mins(5);
    loop {
        if check_liveness(span, liveness) == RunningStatus::Running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Box::new(BootstrapError::StartTimeout(name.to_string())));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn perform(
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
        permissions.as_ref(),
    );
    let state = match state_observer.discover(guard.span(), &douglas_folders) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return;
        }
    };

    let Ok(plan) = resolve_plan(guard.span(), create_plan(state)) else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return;
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

    match execute_plan(guard.span(), plan, &mut context, || ()).await {
        Ok(()) => guard.finish_with_outcome(Outcome::Ok),
        Err(()) => {
            guard.finish_with_outcome(Outcome::Failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BootstrapError, DouglasService, State, create_plan, tag_event_with_service,
        wait_until_running,
    };
    use blueprint::{
        listener::LivenessCheck,
        service::{BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser},
    };
    use log::{Event, EventKind, Level, ScopeId, ScopeKind, Span};
    use std::sync::Arc;

    struct NullReporter;
    impl log::Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    fn service(name: &'static str, liveness: LivenessCheck) -> DouglasService {
        DouglasService {
            name,
            bootstrap_reporting: BootstrapReporting::Pipe,
            liveness,
            definition: ServiceDefinition::new(
                ServiceUser::create_managed(name),
                name,
                Vec::new(),
                BootstrapReporting::Pipe,
            ),
        }
    }

    fn tcp_liveness(port: u16) -> LivenessCheck {
        LivenessCheck::TcpPort {
            host: "127.0.0.1".to_string(),
            port,
        }
    }

    fn step_descriptions(steps: &[Box<dyn blueprint::Command<super::Context<'_>>>]) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_error_when_not_root() {
        let state = State {
            is_root: false,
            ..Default::default()
        };

        let result = create_plan(state);

        assert!(matches!(result, Err(BootstrapError::MustBeRoot)));
    }

    #[test]
    fn test_create_plan_should_produce_no_steps_when_nothing_is_needed() {
        let state = State {
            is_root: true,
            ..Default::default()
        };

        let Ok(steps) = create_plan(state) else {
            panic!("should plan");
        };

        assert!(steps.is_empty());
    }

    #[test]
    fn test_create_plan_should_batch_pipes_starts_and_waits_across_services() {
        let state = State {
            is_root: true,
            services_needing_start: vec![
                (service("bract", tcp_liveness(1)), ServiceState::default()),
                (service("resin", tcp_liveness(2)), ServiceState::default()),
            ],
            ..Default::default()
        };

        let Ok(steps) = create_plan(state) else {
            panic!("should plan");
        };
        let descriptions = step_descriptions(&steps);
        let Some(last_pipe) = descriptions
            .iter()
            .rposition(|d| d.starts_with("Create pipe for"))
        else {
            panic!("expected pipe steps");
        };
        let Some(first_start) = descriptions.iter().position(|d| d.starts_with("Start ")) else {
            panic!("expected start steps");
        };
        let Some(last_start) = descriptions.iter().rposition(|d| d.starts_with("Start ")) else {
            panic!("expected start steps");
        };
        let Some(first_wait) = descriptions.iter().position(|d| d.starts_with("Wait for")) else {
            panic!("expected wait steps");
        };

        assert!(last_pipe < first_start, "all pipes must precede all starts");
        assert!(last_start < first_wait, "all starts must precede all waits");
        assert_eq!(
            descriptions
                .iter()
                .filter(|d| d.starts_with("Start "))
                .count(),
            2
        );
    }

    #[test]
    fn test_tag_event_with_service_should_prefix_scope_started_label() {
        let mut event = Event::start_scope(ScopeId::new(), "Bootstrapping", ScopeKind::Group);

        tag_event_with_service(&mut event, "seedbank");

        assert!(matches!(
            event.kind,
            EventKind::ScopeStarted { ref label, .. } if label == "[seedbank] Bootstrapping"
        ));
    }

    #[test]
    fn test_tag_event_with_service_should_prefix_message_text() {
        let mut event = Event::new(
            ScopeId::new(),
            EventKind::Message {
                level: Level::Info,
                text: "hello".to_string(),
            },
        );

        tag_event_with_service(&mut event, "resin");

        assert!(matches!(
            event.kind,
            EventKind::Message { ref text, .. } if text == "[resin] hello"
        ));
    }

    #[test]
    fn test_tag_event_with_service_should_leave_plan_hint_untouched() {
        let mut event = Event::new(
            ScopeId::new(),
            EventKind::PlanHint {
                steps: vec!["one".to_string()],
            },
        );

        tag_event_with_service(&mut event, "resin");

        assert!(matches!(
            event.kind,
            EventKind::PlanHint { ref steps } if steps == &vec!["one".to_string()]
        ));
    }

    #[tokio::test]
    async fn test_wait_until_running_should_return_immediately_when_already_running() {
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
            panic!("should bind");
        };
        let Ok(local_addr) = listener.local_addr() else {
            panic!("should have local addr");
        };
        let port = local_addr.port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });

        let result = wait_until_running("test-service", &tcp_liveness(port), &span()).await;

        assert!(result.is_ok());
    }
}
