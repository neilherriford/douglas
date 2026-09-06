use crate::blueprints::{
    RequestedBy, agent_container_name, container_name, core_seedling_forbidden_for,
};
use crate::labels;
use async_trait::async_trait;
use blueprint::{
    Command, Step,
    bootstrap::{execute_plan, resolve_plan},
    push_step,
};
use docker::client::ContainerRef;
use docker_types::DockerNameError;
use log::{Reporter, ScopeKind, Span};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StopSeedlingError {
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Cannot stop seedling {0}: it is a core seedling managed by douglas")]
    CoreSeedling(String),
}

struct Context<'a> {
    docker_client: &'a dyn docker::client::Client,
    seedbank_client: &'a dyn seedbank_client::Client,
}

#[derive(Debug)]
struct State {
    container_exists: bool,
    container_is_running: bool,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
    origin: Option<seedbank_types::Origin>,
    agent_container_exists: bool,
    agent_container_is_running: bool,
    agent_container_name: docker_types::ContainerName,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client: &dyn docker::client::Client,
    seedbank_client: &dyn seedbank_client::Client,
    name: &seedbank_types::Name,
    requested_by: RequestedBy,
) -> Result<(), StopSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Stopping seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver::new(docker_client);
        state_observer.discover(guard.span(), name).await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(name, state, requested_by)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            docker_client,
            seedbank_client,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            StopSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a dyn docker::client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(docker_client: &'a dyn docker::client::Client) -> Self {
        Self { docker_client }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        name: &seedbank_types::Name,
    ) -> Result<State, StopSeedlingError> {
        let guard = span
            .create_child(
                "Stopping seedling, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State {
            container_exists: false,
            container_is_running: false,
            container_name: container_name(name)?,
            version: None,
            origin: None,
            agent_container_exists: false,
            agent_container_is_running: false,
            agent_container_name: agent_container_name(name)?,
        };

        result.agent_container_exists = self
            .docker_client
            .container_exists(ContainerRef::FullName(result.agent_container_name.clone()))
            .await?;
        if result.agent_container_exists {
            result.agent_container_is_running = self
                .docker_client
                .container_status(ContainerRef::FullName(result.agent_container_name.clone()))
                .await?
                == docker_types::Status::Running;
        }

        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(result.container_name.clone()))
            .await?
        {
            return Ok(result);
        }
        result.container_exists = true;

        result.container_is_running = self
            .docker_client
            .container_status(ContainerRef::FullName(result.container_name.clone()))
            .await?
            == docker_types::Status::Running;

        let container_labels = self
            .docker_client
            .container_labels(ContainerRef::FullName(result.container_name.clone()))
            .await?;
        result.origin = labels::get_origin(&container_labels);
        result.version = labels::get_version(&container_labels).ok();

        guard.finish(Ok(result))
    }
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    state: State,
    requested_by: RequestedBy,
) -> Result<Vec<Step<Context<'a>>>, StopSeedlingError> {
    let mut steps: Vec<Step<Context>> = Vec::new();

    if state.container_exists && core_seedling_forbidden_for(state.origin, requested_by) {
        return Err(StopSeedlingError::CoreSeedling(name.to_string()));
    }

    push_step(&mut steps, SetDesiredRunStatusToStopped::new(name.clone()));

    if state.container_exists && state.container_is_running {
        push_step(
            &mut steps,
            StopSeedling::new(name.clone(), state.container_name, state.version),
        );
    }

    if state.agent_container_is_running {
        push_step(
            &mut steps,
            StopSeedling::new(name.clone(), state.agent_container_name, None),
        );
    }

    Ok(steps)
}

struct StopSeedling {
    seedling_name: seedbank_types::Name,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
}

impl StopSeedling {
    pub fn new(
        seedling_name: seedbank_types::Name,
        container_name: docker_types::ContainerName,
        version: Option<seedbank_types::Version>,
    ) -> Self {
        Self {
            seedling_name,
            container_name,
            version,
        }
    }
}

impl std::fmt::Display for StopSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.version {
            Some(version) => write!(f, "Stopping seedling '{}' (v{version})", self.seedling_name),
            None => write!(f, "Stopping seedling '{}'", self.seedling_name),
        }
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StopSeedling {
    fn name(&self) -> String {
        "Stopping seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let message = match &self.version {
            Some(version) => format!("Stopping seedling '{}' (v{version})…", self.seedling_name),
            None => format!("Stopping seedling '{}'…", self.seedling_name),
        };
        let guard = span.create_child(&message, ScopeKind::Step).start_guard();

        context
            .docker_client
            .stop_container(ContainerRef::FullName(self.container_name.clone()))
            .await?;

        guard.finish(Ok(()))
    }
}

struct SetDesiredRunStatusToStopped {
    seedling_name: seedbank_types::Name,
}

impl SetDesiredRunStatusToStopped {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for SetDesiredRunStatusToStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Setting seedling '{}' desired running status to stopped",
            self.seedling_name
        )
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for SetDesiredRunStatusToStopped {
    fn name(&self) -> String {
        "Setting desired running state to stopped".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!(
                    "Setting seedling '{}' desired running status to stopped",
                    self.seedling_name
                ),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .set_desired_run_status(
                &self.seedling_name,
                seedbank_types::DesiredRunStatus::Stopped,
            )
            .await?;
        guard.finish(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "traefik".parse().unwrap()
    }

    fn stoppable_state() -> State {
        State {
            container_exists: true,
            container_is_running: true,
            container_name: container_name(&name()).unwrap(),
            version: Some(seedbank_types::Version(1)),
            origin: Some(seedbank_types::Origin::User),
            agent_container_exists: false,
            agent_container_is_running: false,
            agent_container_name: agent_container_name(&name()).unwrap(),
        }
    }

    fn step_descriptions(steps: Vec<Step<Context<'_>>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_stop_a_running_container() {
        let steps = create_plan(&name(), stoppable_state(), RequestedBy::Operator)
            .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to stopped",
                "Stopping seedling 'traefik' (v1)",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_omit_the_version_when_it_could_not_be_determined() {
        let steps = create_plan(
            &name(),
            State {
                version: None,
                ..stoppable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to stopped",
                "Stopping seedling 'traefik'",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_be_a_no_op_when_the_container_does_not_exist() {
        let steps = create_plan(
            &name(),
            State {
                container_exists: false,
                ..stoppable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Setting seedling 'traefik' desired running status to stopped"]
        );
    }

    #[test]
    fn test_create_plan_should_be_a_no_op_when_not_running() {
        let steps = create_plan(
            &name(),
            State {
                container_is_running: false,
                ..stoppable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Setting seedling 'traefik' desired running status to stopped"]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_to_stop_a_core_seedling_for_an_operator() {
        let result = create_plan(
            &name(),
            State {
                origin: Some(seedbank_types::Origin::Core),
                ..stoppable_state()
            },
            RequestedBy::Operator,
        );

        assert!(matches!(result, Err(StopSeedlingError::CoreSeedling(_))));
    }

    #[test]
    fn test_create_plan_should_allow_the_watchdog_to_stop_a_core_seedling() {
        let steps = create_plan(
            &name(),
            State {
                origin: Some(seedbank_types::Origin::Core),
                ..stoppable_state()
            },
            RequestedBy::Watchdog,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to stopped",
                "Stopping seedling 'traefik' (v1)",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_also_stop_a_running_agent_container() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_running: true,
                ..stoppable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to stopped",
                "Stopping seedling 'traefik' (v1)",
                "Stopping seedling 'traefik'",
            ]
        );
    }

    #[test]
    fn test_create_plan_should_skip_stopping_an_agent_container_that_is_not_running() {
        let steps = create_plan(
            &name(),
            State {
                agent_container_exists: true,
                agent_container_is_running: false,
                ..stoppable_state()
            },
            RequestedBy::Operator,
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Setting seedling 'traefik' desired running status to stopped",
                "Stopping seedling 'traefik' (v1)",
            ]
        );
    }
}
