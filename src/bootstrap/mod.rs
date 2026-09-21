use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use blueprint::{Command, listener::LivenessCheck, service::ServiceDefinition};
use bract_types::is_douglas_container;
use config::DouglasFolders;
use docker::client::ClientBuilder;
use file_system::FileReader;
use heartbeat::{HeartbeatReaderFactory, LocalHeartbeatReaderFactory};
use log::{Level, Outcome, ScopeGuard, ScopeKind, Span};
use os::Os;
use thiserror::Error;

pub mod compatibility;
pub mod core_seedlings;
pub mod journal;
pub mod openbao;
pub mod retention;
pub mod rollback;
pub mod staged_copy;
pub mod stop;
pub mod system;
pub mod upgrade;

pub(crate) struct ServiceControl<'a> {
    pub(crate) os: &'a dyn Os,
    pub(crate) heartbeat_reader_factory: &'a dyn HeartbeatReaderFactory,
    pub(crate) bract_client: &'a dyn bract_client::Client,
    pub(crate) docker_client: &'a dyn docker::client::Client,
}

pub(crate) trait HasServiceControl {
    fn service_control(&self) -> &ServiceControl<'_>;
}

pub(crate) struct OwnedServiceControl {
    os: Arc<dyn Os>,
    heartbeat_reader_factory: LocalHeartbeatReaderFactory,
    bract_client: bract_client::UdsClient,
    docker_client: Box<dyn docker::client::Client>,
}

impl OwnedServiceControl {
    pub(crate) async fn build(
        guard: &ScopeGuard,
        os: Arc<dyn Os>,
        douglas_folders: &DouglasFolders,
        file_reader: Arc<dyn FileReader>,
    ) -> Option<Self> {
        let docker_client = match docker::client::UdsClientBuilder
            .build(Arc::clone(&guard.reporter()))
            .await
        {
            Ok(docker_client) => docker_client,
            Err(err) => {
                guard.span().message(
                    Level::Warn,
                    &format!("Failed to create docker client: {err}"),
                );
                guard.finish_with_outcome(Outcome::Failed);
                return None;
            }
        };

        Some(Self {
            os,
            heartbeat_reader_factory: LocalHeartbeatReaderFactory::new(
                douglas_folders.clone(),
                file_reader,
            ),
            bract_client: bract_client::UdsClient::new(
                Arc::clone(&guard.reporter()),
                douglas_folders,
            ),
            docker_client,
        })
    }

    pub(crate) fn borrow(&self) -> ServiceControl<'_> {
        ServiceControl {
            os: self.os.as_ref(),
            heartbeat_reader_factory: &self.heartbeat_reader_factory,
            bract_client: &self.bract_client,
            docker_client: &*self.docker_client,
        }
    }
}

#[derive(Error, Debug)]
pub enum LivenessCheckError {
    #[error("Service '{0}' has no configured liveness check")]
    MissingLivenessCheck(String),
    #[error("Unknown service '{0}'")]
    UnknownService(String),
}

pub(crate) fn liveness_check(
    service_name: &str,
    douglas_folders: &DouglasFolders,
) -> Result<LivenessCheck, LivenessCheckError> {
    let definition = if service_name == config::services::BRACT {
        bract::service_definition(douglas_folders)
    } else if service_name == config::services::SEEDBANK {
        seedbank::service_definition(douglas_folders)
    } else if service_name == config::services::RESIN {
        resin::service_definition(douglas_folders)
    } else if service_name == config::services::WOODWARD {
        woodward::service_definition(douglas_folders)
    } else {
        return Err(LivenessCheckError::UnknownService(service_name.to_string()));
    };

    require_liveness(&definition, service_name)
}

fn require_liveness(
    definition: &ServiceDefinition,
    service_name: &str,
) -> Result<LivenessCheck, LivenessCheckError> {
    definition
        .liveness
        .clone()
        .ok_or_else(|| LivenessCheckError::MissingLivenessCheck(service_name.to_string()))
}

pub(crate) fn kill_service(
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

pub(crate) struct KillService {
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
impl<C: HasServiceControl + Send> Command<C> for KillService {
    fn name(&self) -> String {
        "Kill service".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let control = context.service_control();
        kill_service(
            span,
            &self.service_name,
            control.os,
            control.heartbeat_reader_factory,
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
pub(crate) struct StopBract {
    including_containers: bool,
}

impl StopBract {
    pub fn new(including_containers: bool) -> Self {
        Self {
            including_containers,
        }
    }

    async fn request_bract_stop(
        &self,
        guard: &ScopeGuard,
        bract_client: &dyn bract_client::Client,
        timeout: Duration,
    ) -> bool {
        let outcome =
            match tokio::time::timeout(timeout, bract_client.stop_bract(self.including_containers))
                .await
            {
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
impl<C: HasServiceControl + Send> Command<C> for StopBract {
    fn name(&self) -> String {
        "Stopping bract".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Stopping bract", ScopeKind::Step)
            .start_guard();

        let control = context.service_control();
        let needs_kill = self
            .request_bract_stop(&guard, control.bract_client, BRACT_STOP_TIMEOUT)
            .await;
        if needs_kill {
            if self.including_containers {
                self.try_stop_douglas_containers(&guard, control.docker_client)
                    .await;
            }

            if let Err(err) = kill_service(
                span,
                config::services::BRACT,
                control.os,
                control.heartbeat_reader_factory,
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
