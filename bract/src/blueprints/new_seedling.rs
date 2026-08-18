use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use docker::client::{ClientBuilder, ContainerRef, ImageRef};
use docker_types::DockerNameError;
use log::{Reporter, ScopeKind, Span};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NewSeedlingError {
    #[error("Docker error: {0}")]
    DockerError(#[from] docker::DockerError),
    #[error("Seedbank error: {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
    #[error("Docker name error {0}")]
    DockerNameError(#[from] DockerNameError),
    #[error("Image path component error {0}")]
    ImagePathComponentError(#[from] docker_types::ImagePathComponentError),
    #[error("Resin error: {0}")]
    ResinError(#[from] resin_client::Error),
    #[error("Resin name error: {0}")]
    ResinNameError(#[from] resin_types::NameParseError),
    #[error("Seedling already exists")]
    SeedlingAlreadyExists,
    #[error("Seedling is in an orphaned state.  Delete first before creating")]
    SeedlingOrphaned,
    #[error("'{0}' is a reserved name managed by douglas")]
    ReservedName(String),
    #[error("Mount references unregistered sibling seedlings: {0:?}")]
    MissingMountSiblings(Vec<String>),
}

struct Context<'a> {
    seedbank_client: &'a dyn seedbank_client::Client,
}

#[derive(Debug)]
struct State {
    seedling_exists: bool,
    container_exists: bool,
    image_exists: bool,
    registered: bool,
    image_name: docker_types::ImagePathComponent,
    missing_mount_siblings: Vec<String>,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client_builder: &dyn ClientBuilder,
    resin_client_builder: &dyn resin_client::ClientBuilder,
    seedbank_client: &dyn seedbank_client::Client,
    registry: &docker_types::Registry,
    name: &seedbank_types::Name,
    seedling_spec: &seedbank_types::SeedlingSpec,
) -> Result<String, NewSeedlingError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("Creating seedling '{name}'…"),
        log::ScopeKind::Group,
    )
    .start_guard();

    if seedbank_types::RESERVED_SEEDLING_NAMES.contains(&name.as_ref()) {
        return guard.finish(Err(NewSeedlingError::ReservedName(name.to_string())));
    }

    let mut docker_client = match docker_client_builder.build(Arc::clone(&reporter)).await {
        Ok(docker_client) => docker_client,
        Err(err) => {
            return guard.finish(Err(NewSeedlingError::FailedBoostrap(vec![err.to_string()])));
        }
    };

    let mut resin_client = match resin_client_builder.build(Arc::clone(&reporter)).await {
        Ok(resin_client) => resin_client,
        Err(err) => {
            return guard.finish(Err(NewSeedlingError::FailedBoostrap(vec![err.to_string()])));
        }
    };

    let state = {
        let mut state_observer = StateObserver::new(
            &mut *docker_client,
            registry,
            seedbank_client,
            &mut *resin_client,
        );
        state_observer
            .discover(guard.span(), name, seedling_spec)
            .await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(name, seedling_spec, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    {
        let mut context = Context { seedbank_client };
        if let Err(err) = execute_plan(guard.span(), plan, &mut context, |reason| {
            NewSeedlingError::FailedBoostrap(vec![reason])
        })
        .await
        {
            return guard.finish(Err(err));
        }
    }

    guard.finish(Ok(format!(
        "Ready. If your image isn't already named for this registry, tag it first: \
         `docker tag {name} {registry}/{name}`. Then push it to {registry}/{name} to deploy it."
    )))
}

struct StateObserver<'a> {
    docker_client: &'a mut dyn docker::client::Client,
    registry: &'a docker_types::Registry,
    seedbank_client: &'a dyn seedbank_client::Client,
    resin_client: &'a mut dyn resin_client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        docker_client: &'a mut dyn docker::client::Client,
        registry: &'a docker_types::Registry,
        seedbank_client: &'a dyn seedbank_client::Client,
        resin_client: &'a mut dyn resin_client::Client,
    ) -> Self {
        Self {
            docker_client,
            registry,
            seedbank_client,
            resin_client,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        name: &seedbank_types::Name,
        seedling_spec: &seedbank_types::SeedlingSpec,
    ) -> Result<State, NewSeedlingError> {
        let guard = span
            .create_child(
                "Dropping seedling, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let container_name = name.as_ref().parse::<docker_types::ContainerName>()?;
        let image_name = name.as_ref().parse::<docker_types::ImagePathComponent>()?;

        let mut result = State {
            seedling_exists: false,
            container_exists: false,
            image_exists: false,
            registered: false,
            image_name: image_name.clone(),
            missing_mount_siblings: Vec::new(),
        };

        if self.seedbank_client.exists(name).await? {
            result.seedling_exists = true;
            guard.finish_with_outcome(log::Outcome::Ok);
            return Ok(result);
        }

        if self
            .docker_client
            .container_exists(ContainerRef::FullName(container_name))
            .await?
        {
            result.container_exists = true;
            guard.finish_with_outcome(log::Outcome::Ok);
            return Ok(result);
        }

        if self
            .docker_client
            .image_exists(
                self.registry,
                ImageRef::VersionedName(docker_types::VersionedImageName {
                    namespace: None,
                    name: image_name,
                    version: docker_types::Version::Latest,
                }),
            )
            .await?
        {
            result.container_exists = true;
            guard.finish_with_outcome(log::Outcome::Ok);
            return Ok(result);
        }

        let resin_name: resin_types::Name = name.as_ref().parse()?;

        if self.resin_client.repository_registered(&resin_name).await? {
            result.registered = true;
        }

        for mount in seedling_spec.mounts.values() {
            let seedbank_types::MountType::PersistedShared(siblings) = mount.kind() else {
                continue;
            };
            for sibling in siblings {
                if !self.seedbank_client.exists(sibling).await? {
                    result.missing_mount_siblings.push(sibling.to_string());
                }
            }
        }

        guard.finish(Ok(result))
    }
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

fn create_plan<'a>(
    name: &seedbank_types::Name,
    seedling_spec: &seedbank_types::SeedlingSpec,
    state: State,
) -> Result<Vec<Step<'a>>, NewSeedlingError> {
    let mut steps: Vec<Box<dyn Command<Context>>> = Vec::new();

    if state.seedling_exists {
        return Err(NewSeedlingError::SeedlingAlreadyExists);
    }

    if state.container_exists || state.image_exists || state.registered {
        return Err(NewSeedlingError::SeedlingOrphaned);
    }

    if !state.missing_mount_siblings.is_empty() {
        return Err(NewSeedlingError::MissingMountSiblings(
            state.missing_mount_siblings,
        ));
    }

    push_step(
        &mut steps,
        NewSeedlingFromSpec::new(
            name.clone(),
            state.image_name.clone(),
            seedling_spec.clone(),
        ),
    );

    push_step(&mut steps, ClaimRoot::new(name.clone()));

    Ok(steps)
}

struct NewSeedlingFromSpec {
    seedling_name: seedbank_types::Name,
    image_name: docker_types::ImagePathComponent,
    seedling_spec: seedbank_types::SeedlingSpec,
}

impl NewSeedlingFromSpec {
    pub fn new(
        seedling_name: seedbank_types::Name,
        image_name: docker_types::ImagePathComponent,
        seedling_spec: seedbank_types::SeedlingSpec,
    ) -> Self {
        Self {
            seedling_name,
            image_name,
            seedling_spec,
        }
    }
}

impl std::fmt::Display for NewSeedlingFromSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Creating seedling for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for NewSeedlingFromSpec {
    fn name(&self) -> String {
        "Creating seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Creating seedling for '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        let image = docker_types::VersionedImageName {
            namespace: None,
            name: self.image_name.clone(),
            version: docker_types::Version::Latest,
        };
        let definition = seedbank_types::SeedlingDefinition::new(
            image,
            self.seedling_spec.mounts.clone(),
            seedbank_types::Routing::Routed {
                route: seedbank_types::RouteSpec::Root,
                ports: self.seedling_spec.ports.clone(),
            },
        );

        context
            .seedbank_client
            .create(
                &self.seedling_name,
                &seedbank_types::Version(1),
                &definition,
            )
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

struct ClaimRoot {
    seedling_name: seedbank_types::Name,
}

impl ClaimRoot {
    pub fn new(seedling_name: seedbank_types::Name) -> Self {
        Self { seedling_name }
    }
}

impl std::fmt::Display for ClaimRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Claiming root for '{}'", self.seedling_name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ClaimRoot {
    fn name(&self) -> String {
        "Claiming root".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Claiming root for '{}'", self.seedling_name),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .seedbank_client
            .claim_root(&self.seedling_name)
            .await?;

        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> seedbank_types::Name {
        "foo".parse().expect("valid name")
    }

    fn seedling_spec() -> seedbank_types::SeedlingSpec {
        seedbank_types::SeedlingSpec::new(
            std::collections::HashMap::new(),
            seedbank_types::PortSpec {
                public: 8080,
                additional: Vec::new(),
            },
        )
    }

    fn state() -> State {
        State {
            seedling_exists: false,
            container_exists: false,
            image_exists: false,
            registered: false,
            image_name: "foo".parse().expect("valid image name"),
            missing_mount_siblings: Vec::new(),
        }
    }

    fn step_descriptions(steps: Vec<Step<'_>>) -> Vec<String> {
        steps.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_create_plan_should_create_the_seedling_and_claim_root() {
        let steps = create_plan(&name(), &seedling_spec(), state()).expect("should produce a plan");

        assert_eq!(
            step_descriptions(steps),
            vec![
                "Creating seedling for 'foo'".to_string(),
                "Claiming root for 'foo'".to_string(),
            ]
        );
    }

    #[test]
    fn test_create_plan_should_refuse_when_the_seedling_already_exists() {
        let result = create_plan(
            &name(),
            &seedling_spec(),
            State {
                seedling_exists: true,
                ..state()
            },
        );

        assert!(matches!(
            result,
            Err(NewSeedlingError::SeedlingAlreadyExists)
        ));
    }

    #[test]
    fn test_create_plan_should_refuse_when_orphaned() {
        let result = create_plan(
            &name(),
            &seedling_spec(),
            State {
                container_exists: true,
                ..state()
            },
        );

        assert!(matches!(result, Err(NewSeedlingError::SeedlingOrphaned)));
    }

    #[test]
    fn test_create_plan_should_refuse_when_mount_siblings_are_missing() {
        let result = create_plan(
            &name(),
            &seedling_spec(),
            State {
                missing_mount_siblings: vec!["bar".to_string()],
                ..state()
            },
        );

        assert!(matches!(
            result,
            Err(NewSeedlingError::MissingMountSiblings(siblings)) if siblings == vec!["bar".to_string()]
        ));
    }
}
