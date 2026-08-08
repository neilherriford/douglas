use crate::blueprints::container_name;
use crate::labels;
use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use docker::client::{ClientBuilder, ContainerRef};
use docker_types::DockerNameError;
use log::{Reporter, ScopeKind, Span};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DropSeedlingError {
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Cannot drop seedling {0}")]
    CannotDropSeedling(String),
    #[error("Cannot drop seedling {0}: it is a core seedling managed by douglas")]
    CoreSeedling(String),
}

struct Context<'a> {
    docker_client: &'a mut dyn docker::client::Client,
}

#[derive(Debug)]
struct State {
    container_exists: bool,
    container_is_stopped: bool,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
    origin: Option<labels::Origin>,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client_builder: &dyn ClientBuilder,
    name: &seedbank_types::Name,
) -> Result<(), DropSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Dropping seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut docker_client = match docker_client_builder.build(Arc::clone(&reporter)).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(DropSeedlingError::FailedBoostrap(vec![
                err.to_string(),
            ])));
        }
    };

    let state = {
        let mut state_observer = StateObserver::new(&mut *docker_client);
        state_observer.discover(guard.span(), name).await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(name, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let result = {
        let mut context = Context {
            docker_client: &mut *docker_client,
        };
        execute_plan(guard.span(), plan, &mut context, |reason| {
            DropSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
    };

    guard.finish(result)
}

struct StateObserver<'a> {
    docker_client: &'a mut dyn docker::client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(docker_client: &'a mut dyn docker::client::Client) -> Self {
        Self { docker_client }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        name: &seedbank_types::Name,
    ) -> Result<State, DropSeedlingError> {
        let guard = span
            .create_child(
                "Dropping seedling, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State {
            container_exists: false,
            container_is_stopped: false,
            container_name: container_name(name)?,
            version: None,
            origin: None,
        };

        if !self
            .docker_client
            .container_exists(ContainerRef::FullName(result.container_name.clone()))
            .await?
        {
            return Ok(result);
        }
        result.container_exists = true;
        result.container_is_stopped = matches!(
            self.docker_client
                .container_status(ContainerRef::FullName(result.container_name.clone()))
                .await?,
            docker_types::Status::Created
                | docker_types::Status::Exited
                | docker_types::Status::Dead
        );

        let container_labels = self
            .docker_client
            .container_labels(ContainerRef::FullName(result.container_name.clone()))
            .await?;
        result.origin = labels::get_origin(&container_labels);
        result.version = labels::get_version(&container_labels).ok();

        guard.finish(Ok(result))
    }
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    state: State,
) -> Result<Vec<Step<'a>>, DropSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

    if !state.container_exists {
        return Err(DropSeedlingError::CannotDropSeedling(
            "Seedling not started".to_string(),
        ));
    }
    if state.origin == Some(labels::Origin::Core) {
        return Err(DropSeedlingError::CoreSeedling(name.to_string()));
    }
    if !state.container_is_stopped {
        return Err(DropSeedlingError::CannotDropSeedling(
            "Seedling is not stopped".to_string(),
        ));
    }

    push_step(
        &mut steps,
        DropSeedling::new(name.clone(), state.container_name, state.version),
    );

    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "traefik".parse().unwrap()
    }

    fn droppable_state() -> State {
        State {
            container_exists: true,
            container_is_stopped: true,
            container_name: container_name(&name()).unwrap(),
            version: Some(seedbank_types::Version(1)),
            origin: Some(labels::Origin::User),
        }
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_drop_a_stopped_container() {
        let steps = create_plan(&name(), droppable_state()).expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Dropping seedling 'traefik' (v1)"]
        );
    }

    #[test]
    fn test_create_plan_should_omit_the_version_when_it_could_not_be_determined() {
        let steps = create_plan(
            &name(),
            State {
                version: None,
                ..droppable_state()
            },
        )
        .expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec!["Dropping seedling 'traefik'"]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_container_does_not_exist() {
        let result = create_plan(
            &name(),
            State {
                container_exists: false,
                ..droppable_state()
            },
        );

        assert!(matches!(
            result,
            Err(DropSeedlingError::CannotDropSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_when_not_stopped() {
        let result = create_plan(
            &name(),
            State {
                container_is_stopped: false,
                ..droppable_state()
            },
        );

        assert!(matches!(
            result,
            Err(DropSeedlingError::CannotDropSeedling(_))
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_to_drop_a_core_seedling() {
        let result = create_plan(
            &name(),
            State {
                origin: Some(labels::Origin::Core),
                ..droppable_state()
            },
        );

        assert!(matches!(result, Err(DropSeedlingError::CoreSeedling(_))));
    }
}

struct DropSeedling {
    seedling_name: seedbank_types::Name,
    container_name: docker_types::ContainerName,
    version: Option<seedbank_types::Version>,
}

impl DropSeedling {
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

impl std::fmt::Display for DropSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.version {
            Some(version) => write!(f, "Dropping seedling '{}' (v{version})", self.seedling_name),
            None => write!(f, "Dropping seedling '{}'", self.seedling_name),
        }
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for DropSeedling {
    fn name(&self) -> String {
        "Dropping seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let message = match &self.version {
            Some(version) => format!("Dropping seedling '{}' (v{version})…", self.seedling_name),
            None => format!("Dropping seedling '{}'…", self.seedling_name),
        };
        let guard = span.create_child(&message, ScopeKind::Step).start_guard();

        context
            .docker_client
            .delete_container(ContainerRef::FullName(self.container_name.clone()))
            .await?;

        guard.finish(Ok(()))
    }
}
