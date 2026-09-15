use crate::bootstrap::{LivenessCheckError, liveness_check};
use async_trait::async_trait;
use blueprint::{
    Command, RunningStatus,
    bootstrap::{execute_plan, resolve_plan},
    listener::{LivenessCheck, check_liveness},
    push_step,
};
use bract_types::is_douglas_container;
use config::DouglasFolders;
use credentials::Credentials;
use docker::client::ClientBuilder;
use file_system::{FileReader, FileSystemError};
use log::{Level, Outcome, Reporter, ScopeGuard, ScopeKind, Span};
use os::Os;
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use woodward::{HeartbeatReaderFactory, LocalHeartbeatReaderFactory};

#[derive(Error, Debug)]
pub enum StopError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Liveness check error: {0}")]
    LivenessCheckError(#[from] LivenessCheckError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

struct Context<'a> {
    os: &'a dyn Os,
    heartbeat_reader_factory: &'a dyn HeartbeatReaderFactory,
    bract_client: &'a dyn bract_client::Client,
    docker_client: &'a dyn docker::client::Client,
}

#[derive(Default)]
struct State {
    is_root: bool,
    woodward_running_status: RunningStatus,
    resin_running_status: RunningStatus,
    seedbank_running_status: RunningStatus,
    bract_running_status: RunningStatus,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    bract_liveness_check: &'a LivenessCheck,
    seedbank_liveness_check: &'a LivenessCheck,
    resin_liveness_check: &'a LivenessCheck,
    woodward_liveness_check: &'a LivenessCheck,
}

impl StateObserver<'_> {
    pub fn discover(&mut self, span: &Span) -> Result<State, StopError> {
        let guard = span
            .create_child(
                "Stopping douglas system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        if !self.credentials.is_root() {
            return guard.finish(Ok(State::default()));
        }

        let result = State {
            is_root: true,
            bract_running_status: check_liveness(guard.span(), self.bract_liveness_check),
            seedbank_running_status: check_liveness(guard.span(), self.seedbank_liveness_check),
            resin_running_status: check_liveness(guard.span(), self.resin_liveness_check),
            woodward_running_status: check_liveness(guard.span(), self.woodward_liveness_check),
        };

        guard.finish(Ok(result))
    }
}

fn create_plan<'a>(state: &State) -> Result<Vec<Step<'a>>, StopError> {
    if !state.is_root {
        return Err(StopError::MustBeRoot);
    }

    let mut result = Vec::new();

    if matches!(state.woodward_running_status, RunningStatus::Running) {
        push_step(&mut result, KillService::new(config::services::WOODWARD));
    }

    if matches!(state.bract_running_status, RunningStatus::Running) {
        push_step(&mut result, StopBract::default());
    }

    if matches!(state.seedbank_running_status, RunningStatus::Running) {
        push_step(&mut result, KillService::new(config::services::SEEDBANK));
    }

    if matches!(state.resin_running_status, RunningStatus::Running) {
        push_step(&mut result, KillService::new(config::services::RESIN));
    }

    Ok(result)
}

fn kill_service(
    span: &Span,
    service_name: &str,
    os: &dyn Os,
    heartbeat_reader_factory: &dyn HeartbeatReaderFactory,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let guard = span
        .create_child(&format!("Killing service {service_name}…"), ScopeKind::Step)
        .start_guard();

    let heartbeat_reader = heartbeat_reader_factory.create(service_name);

    let pid = match heartbeat_reader.read() {
        Ok(heartbeat) => heartbeat.pid,
        Err(err) => return guard.finish(Err(Box::new(err))),
    };

    match os.kill(pid) {
        Ok(()) => guard.finish(Ok(())),
        Err(err) => guard.finish(Err(Box::new(err))),
    }
}

struct KillService {
    service_name: String,
}

impl KillService {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
        }
    }
}

impl std::fmt::Display for KillService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kill service {}", self.service_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for KillService {
    fn name(&self) -> String {
        "Kill service".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        kill_service(
            span,
            &self.service_name,
            context.os,
            context.heartbeat_reader_factory,
        )
    }
}

const BRACT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

enum BractStopOutcome {
    Stopped,
    Failed(bract_client::Error),
    TimedOut,
}

#[derive(Debug, Default)]
struct StopBract {}

impl StopBract {
    async fn request_bract_stop(
        &self,
        guard: &ScopeGuard,
        bract_client: &dyn bract_client::Client,
        timeout: Duration,
    ) -> bool {
        let outcome = match tokio::time::timeout(timeout, bract_client.stop()).await {
            Ok(Ok(())) => BractStopOutcome::Stopped,
            Ok(Err(err)) => BractStopOutcome::Failed(err),
            Err(_) => BractStopOutcome::TimedOut,
        };

        Self::handle_stop_outcome(guard, outcome)
    }

    fn handle_stop_outcome(guard: &ScopeGuard, outcome: BractStopOutcome) -> bool {
        match outcome {
            BractStopOutcome::Stopped => false,
            BractStopOutcome::Failed(err) => {
                guard
                    .span()
                    .message(Level::Warn, &format!("Bract stop request failed: {err}"));
                true
            }
            BractStopOutcome::TimedOut => {
                guard
                    .span()
                    .message(Level::Warn, "Bract stop request timed out");
                true
            }
        }
    }

    async fn try_stop_douglas_containers(
        &self,
        guard: &ScopeGuard,
        docker_client: &dyn docker::client::Client,
    ) {
        let douglas_containers: Vec<docker_types::ContainerName> =
            match docker_client.list_containers().await {
                Ok(containers) => containers
                    .iter()
                    .filter(|name| is_douglas_container(name))
                    .cloned()
                    .collect(),
                Err(err) => {
                    guard.span().message(
                        Level::Warn,
                        &format!("Failed to list Docker containers during fall back stop: {err}"),
                    );
                    return;
                }
            };

        for douglas_container in douglas_containers {
            self.try_stop_container(guard, docker_client, &douglas_container)
                .await;
        }
    }

    async fn try_stop_container(
        &self,
        guard: &ScopeGuard,
        docker_client: &dyn docker::client::Client,
        douglas_container: &docker_types::ContainerName,
    ) {
        let needs_stop = match docker_client
            .container_status(docker::client::ContainerRef::FullName(
                douglas_container.clone(),
            ))
            .await
        {
            Ok(status) => matches!(status, docker_types::Status::Running),
            Err(err) => {
                guard.span().message(
                    Level::Warn,
                    &format!("Failed to get status for container {douglas_container}: {err}"),
                );
                return;
            }
        };

        if !needs_stop {
            return;
        }

        guard.span().message(
            Level::Info,
            &format!("Stopping container {douglas_container}…"),
        );

        if let Err(err) = docker_client
            .stop_container(docker::client::ContainerRef::FullName(
                douglas_container.clone(),
            ))
            .await
        {
            guard.span().message(
                Level::Warn,
                &format!("Failed to stop container {douglas_container}: {err}"),
            );
        }
    }
}

impl std::fmt::Display for StopBract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stopping Bract")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StopBract {
    fn name(&self) -> String {
        "Stopping bract".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Stopping bract", ScopeKind::Step)
            .start_guard();

        let needs_kill = self
            .request_bract_stop(&guard, context.bract_client, BRACT_STOP_TIMEOUT)
            .await;
        if needs_kill {
            self.try_stop_douglas_containers(&guard, context.docker_client)
                .await;

            if let Err(err) = kill_service(
                span,
                config::services::BRACT,
                context.os,
                context.heartbeat_reader_factory,
            ) {
                guard
                    .span()
                    .message(Level::Warn, &format!("Failed to kill bract: {err}"));
                return guard.finish(Err(err));
            }
        }

        guard.finish(Ok(()))
    }
}

pub(crate) struct Dependencies {
    pub credentials: Arc<dyn Credentials>,
    pub os: Arc<dyn Os>,
    pub douglas_folders: DouglasFolders,
    pub file_reader: Arc<dyn FileReader>,
}

fn try_fetch_liveness_check(
    guard: &ScopeGuard,
    service_name: &str,
    douglas_folders: &DouglasFolders,
) -> Option<LivenessCheck> {
    match liveness_check(service_name, douglas_folders) {
        Ok(check) => Some(check),
        Err(err) => {
            guard.span().message(
                Level::Warn,
                &format!("Failed to fetch liveness check for service {service_name}: {err}"),
            );
            guard.finish_with_outcome(Outcome::Failed);
            None
        }
    }
}

pub async fn perform(reporter: Arc<dyn Reporter>, plan_only: bool, deps: Dependencies) -> bool {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Stopping douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let Some(bract_liveness_check) =
        &try_fetch_liveness_check(&guard, config::services::BRACT, &deps.douglas_folders)
    else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return false;
    };

    let Some(seedbank_liveness_check) =
        &try_fetch_liveness_check(&guard, config::services::SEEDBANK, &deps.douglas_folders)
    else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return false;
    };

    let Some(resin_liveness_check) =
        &try_fetch_liveness_check(&guard, config::services::RESIN, &deps.douglas_folders)
    else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return false;
    };

    let Some(woodward_liveness_check) =
        &try_fetch_liveness_check(&guard, config::services::WOODWARD, &deps.douglas_folders)
    else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return false;
    };

    let mut state_observer = StateObserver {
        credentials: deps.credentials.as_ref(),
        bract_liveness_check,
        seedbank_liveness_check,
        resin_liveness_check,
        woodward_liveness_check,
    };
    let state = match state_observer.discover(guard.span()) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return false;
        }
    };

    let bract_client =
        bract_client::UdsClient::new(Arc::clone(&guard.reporter()), &deps.douglas_folders);

    let heartbeat_reader_factory = LocalHeartbeatReaderFactory::new(
        deps.douglas_folders.clone(),
        Arc::clone(&deps.file_reader),
    );

    let builder = docker::client::UdsClientBuilder;
    let docker_client = match builder.build(Arc::clone(&guard.reporter())).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            guard.span().message(
                Level::Warn,
                &format!("Failed to create docker client: {err}"),
            );
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
    };

    let plan = match resolve_plan(guard.span(), create_plan(&state)) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
    };

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return true;
    }

    let mut context = Context {
        os: deps.os.as_ref(),
        bract_client: &bract_client,
        heartbeat_reader_factory: &heartbeat_reader_factory,
        docker_client: &*docker_client,
    };

    let result = execute_plan(guard.span(), plan, &mut context, |_reason| ()).await;

    if result.is_ok() {
        guard.finish_with_outcome(Outcome::Ok);
        true
    } else {
        guard.finish_with_outcome(Outcome::Failed);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docker::client::ContainerRef;
    use os::MockOs;
    use std::sync::Mutex;
    use woodward::{Heartbeat, HeartbeatReaderError};

    struct CapturingReporter {
        messages: Mutex<Vec<String>>,
    }

    impl CapturingReporter {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                messages: Mutex::new(Vec::new()),
            })
        }

        fn messages(&self) -> Vec<String> {
            let Ok(messages) = self.messages.lock() else {
                panic!("messages mutex poisoned");
            };
            messages.clone()
        }
    }

    impl Reporter for CapturingReporter {
        fn emit(&self, event: log::Event) {
            if let log::EventKind::Message { text, .. } = event.kind {
                let Ok(mut messages) = self.messages.lock() else {
                    panic!("messages mutex poisoned");
                };
                messages.push(text);
            }
        }
    }

    fn test_guard(reporter: Arc<dyn Reporter>) -> ScopeGuard {
        Span::new(reporter, "test", ScopeKind::Group).start_guard()
    }

    fn container_name(raw: &str) -> docker_types::ContainerName {
        let Ok(name) = raw.parse() else {
            panic!("invalid container name: {raw}");
        };
        name
    }

    struct FnHeartbeatReader<F>(F)
    where
        F: Fn() -> Result<Heartbeat, HeartbeatReaderError> + Send + Sync;

    impl<F> woodward::HeartbeatReader for FnHeartbeatReader<F>
    where
        F: Fn() -> Result<Heartbeat, HeartbeatReaderError> + Send + Sync,
    {
        fn read(&self) -> Result<Heartbeat, HeartbeatReaderError> {
            (self.0)()
        }
    }

    fn heartbeat_reader_factory<F>(read: F) -> woodward::MockHeartbeatReaderFactory
    where
        F: Fn() -> Result<Heartbeat, HeartbeatReaderError> + Send + Sync + Clone + 'static,
    {
        let mut factory = woodward::MockHeartbeatReaderFactory::new();
        factory.expect_create().returning(move |_service_name| {
            let read = read.clone();
            Box::new(FnHeartbeatReader(read))
        });
        factory
    }

    fn alive_heartbeat_reader_factory(pid: u32) -> woodward::MockHeartbeatReaderFactory {
        heartbeat_reader_factory(move || {
            Ok(Heartbeat {
                pid,
                written_at: std::time::SystemTime::now(),
            })
        })
    }

    mod create_plan_tests {
        use super::*;

        #[test]
        fn test_should_error_when_not_root() {
            let state = State {
                is_root: false,
                ..Default::default()
            };

            let result = create_plan(&state);

            assert!(matches!(result, Err(StopError::MustBeRoot)));
        }

        #[test]
        fn test_should_produce_no_steps_when_nothing_is_running() {
            let state = State {
                is_root: true,
                ..Default::default()
            };

            let Ok(steps) = create_plan(&state) else {
                panic!("should plan");
            };

            assert!(steps.is_empty());
        }

        #[test]
        fn test_should_order_woodward_then_bract_then_seedbank_then_resin_when_everything_is_running()
         {
            let state = State {
                is_root: true,
                woodward_running_status: RunningStatus::Running,
                bract_running_status: RunningStatus::Running,
                seedbank_running_status: RunningStatus::Running,
                resin_running_status: RunningStatus::Running,
            };

            let Ok(steps) = create_plan(&state) else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert_eq!(
                descriptions,
                vec![
                    "Kill service woodward".to_string(),
                    "Stopping Bract".to_string(),
                    "Kill service seedbank".to_string(),
                    "Kill service resin".to_string(),
                ]
            );
        }

        #[test]
        fn test_should_only_include_steps_for_services_that_are_running() {
            let state = State {
                is_root: true,
                woodward_running_status: RunningStatus::Running,
                bract_running_status: RunningStatus::NotRunning,
                seedbank_running_status: RunningStatus::Running,
                resin_running_status: RunningStatus::NotRunning,
            };

            let Ok(steps) = create_plan(&state) else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert_eq!(
                descriptions,
                vec![
                    "Kill service woodward".to_string(),
                    "Kill service seedbank".to_string(),
                ]
            );
        }
    }

    mod kill_service_tests {
        use super::*;

        #[tokio::test]
        async fn test_should_kill_the_pid_read_from_the_heartbeat() {
            let mut os = MockOs::new();
            os.expect_kill()
                .withf(|pid| *pid == 4242)
                .returning(|_| Ok(()));

            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let factory = heartbeat_reader_factory(|| {
                Ok(Heartbeat {
                    pid: 4242,
                    written_at: std::time::SystemTime::now(),
                })
            });

            let result = kill_service(guard.span(), "bract", &os, &factory);

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_should_fail_when_the_heartbeat_cannot_be_read() {
            let os = MockOs::new();
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let factory = heartbeat_reader_factory(|| {
                Err(HeartbeatReaderError::FileSystemError(
                    file_system::FileSystemError::NotFoundError(std::path::PathBuf::from(
                        "/run/douglas/bract/heartbeat",
                    )),
                ))
            });

            let result = kill_service(guard.span(), "bract", &os, &factory);

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_should_fail_when_the_kill_syscall_fails() {
            let mut os = MockOs::new();
            os.expect_kill()
                .returning(|_| Err(os::OsError::PidTooLarge));

            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let factory = heartbeat_reader_factory(|| {
                Ok(Heartbeat {
                    pid: 4242,
                    written_at: std::time::SystemTime::now(),
                })
            });

            let result = kill_service(guard.span(), "bract", &os, &factory);

            assert!(result.is_err());
        }
    }

    mod handle_stop_outcome_tests {
        use super::*;

        #[test]
        fn test_stopped_should_not_need_a_kill() {
            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);

            let needs_kill = StopBract::handle_stop_outcome(&guard, BractStopOutcome::Stopped);

            assert!(!needs_kill);
            assert!(reporter.messages().is_empty());
        }

        #[test]
        fn test_failed_should_need_a_kill_and_log_the_error() {
            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);

            let needs_kill = StopBract::handle_stop_outcome(
                &guard,
                BractStopOutcome::Failed(bract_client::Error::MissingSocket),
            );

            assert!(needs_kill);
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Bract stop request failed"))
            );
        }

        #[test]
        fn test_timed_out_should_need_a_kill_and_log_a_timeout_message() {
            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);

            let needs_kill = StopBract::handle_stop_outcome(&guard, BractStopOutcome::TimedOut);

            assert!(needs_kill);
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Bract stop request timed out"))
            );
        }
    }

    mod request_bract_stop_tests {
        use super::*;

        #[tokio::test]
        async fn test_should_return_false_when_bract_acknowledges() {
            let mut bract_client = bract_client::MockClient::new();
            bract_client.expect_stop().returning(|| Ok(()));

            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let stop_bract = StopBract::default();

            let needs_kill = stop_bract
                .request_bract_stop(&guard, &bract_client, Duration::from_secs(5))
                .await;

            assert!(!needs_kill);
        }

        #[tokio::test]
        async fn test_should_return_true_when_bract_reports_an_error() {
            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop()
                .returning(|| Err(bract_client::Error::MissingSocket));

            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);
            let stop_bract = StopBract::default();

            let needs_kill = stop_bract
                .request_bract_stop(&guard, &bract_client, Duration::from_secs(5))
                .await;

            assert!(needs_kill);
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Bract stop request failed"))
            );
        }
    }

    mod try_stop_container_tests {
        use super::*;

        #[tokio::test]
        async fn test_should_stop_a_running_container() {
            let name = container_name("doug.traefik");
            let mut docker_client = docker::MockClient::new();
            docker_client
                .expect_container_status()
                .returning(|_| Ok(docker_types::Status::Running));
            docker_client
                .expect_stop_container()
                .withf(move |container_ref| {
                    matches!(container_ref, ContainerRef::FullName(actual) if actual == &container_name("doug.traefik"))
                })
                .returning(|_| Ok(()));

            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);
            let stop_bract = StopBract::default();

            stop_bract
                .try_stop_container(&guard, &docker_client, &name)
                .await;

            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Stopping container"))
            );
        }

        #[tokio::test]
        async fn test_should_leave_a_non_running_container_alone() {
            let name = container_name("doug.traefik");
            let mut docker_client = docker::MockClient::new();
            docker_client
                .expect_container_status()
                .returning(|_| Ok(docker_types::Status::Exited));
            docker_client.expect_stop_container().times(0);

            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let stop_bract = StopBract::default();

            stop_bract
                .try_stop_container(&guard, &docker_client, &name)
                .await;
        }

        #[tokio::test]
        async fn test_should_log_and_return_when_the_status_check_fails() {
            let name = container_name("doug.traefik");
            let mut docker_client = docker::MockClient::new();
            docker_client.expect_container_status().returning(|_| {
                Err(docker::DockerError::FailedToCreateClient(
                    "boom".to_string(),
                ))
            });
            docker_client.expect_stop_container().times(0);

            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);
            let stop_bract = StopBract::default();

            stop_bract
                .try_stop_container(&guard, &docker_client, &name)
                .await;

            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Failed to get status"))
            );
        }
    }

    mod try_stop_douglas_containers_tests {
        use super::*;

        #[tokio::test]
        async fn test_should_only_consider_douglas_prefixed_containers() {
            let mut docker_client = docker::MockClient::new();
            docker_client.expect_list_containers().returning(|| {
                Ok(vec![
                    container_name("doug.traefik"),
                    container_name("doug-agent.traefik"),
                    container_name("some-other-container"),
                ])
            });
            docker_client
                .expect_container_status()
                .times(2)
                .returning(|_| Ok(docker_types::Status::Running));
            docker_client
                .expect_stop_container()
                .times(2)
                .returning(|_| Ok(()));

            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);
            let stop_bract = StopBract::default();

            stop_bract
                .try_stop_douglas_containers(&guard, &docker_client)
                .await;
        }

        #[tokio::test]
        async fn test_should_log_and_return_when_listing_fails() {
            let mut docker_client = docker::MockClient::new();
            docker_client.expect_list_containers().returning(|| {
                Err(docker::DockerError::FailedToCreateClient(
                    "boom".to_string(),
                ))
            });

            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);
            let stop_bract = StopBract::default();

            stop_bract
                .try_stop_douglas_containers(&guard, &docker_client)
                .await;

            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Failed to list Docker containers"))
            );
        }
    }

    mod command_integration_tests {
        use super::*;

        #[tokio::test]
        async fn test_kill_service_run_should_kill_the_named_services_pid() {
            let mut os = MockOs::new();
            os.expect_kill()
                .withf(|pid| *pid == 4242)
                .returning(|_| Ok(()));

            let bract_client = bract_client::MockClient::new();
            let docker_client = docker::MockClient::new();
            let heartbeat_reader_factory = alive_heartbeat_reader_factory(4242);
            let mut context = Context {
                os: &os,
                heartbeat_reader_factory: &heartbeat_reader_factory,
                bract_client: &bract_client,
                docker_client: &docker_client,
            };

            let mut command = KillService::new(config::services::SEEDBANK);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_stop_bract_run_should_do_nothing_further_when_bract_acknowledges() {
            let os = MockOs::new();
            let mut bract_client = bract_client::MockClient::new();
            bract_client.expect_stop().returning(|| Ok(()));
            let docker_client = docker::MockClient::new();

            let heartbeat_reader_factory = alive_heartbeat_reader_factory(4242);
            let mut context = Context {
                os: &os,
                heartbeat_reader_factory: &heartbeat_reader_factory,
                bract_client: &bract_client,
                docker_client: &docker_client,
            };

            let mut command = StopBract::default();
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_stop_bract_run_should_escalate_to_docker_fallback_and_kill_when_bract_does_not_acknowledge()
         {
            let mut os = MockOs::new();
            os.expect_kill().returning(|_| Ok(()));

            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop()
                .returning(|| Err(bract_client::Error::MissingSocket));

            let mut docker_client = docker::MockClient::new();
            docker_client
                .expect_list_containers()
                .returning(|| Ok(vec![container_name("doug.traefik")]));
            docker_client
                .expect_container_status()
                .returning(|_| Ok(docker_types::Status::Running));
            docker_client.expect_stop_container().returning(|_| Ok(()));

            let heartbeat_reader_factory = alive_heartbeat_reader_factory(4242);
            let mut context = Context {
                os: &os,
                heartbeat_reader_factory: &heartbeat_reader_factory,
                bract_client: &bract_client,
                docker_client: &docker_client,
            };

            let mut command = StopBract::default();
            let reporter = CapturingReporter::new();
            let span = Span::new(
                Arc::clone(&reporter) as Arc<dyn Reporter>,
                "test",
                ScopeKind::Group,
            );

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Bract stop request failed"))
            );
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Stopping container"))
            );
        }
    }
}
