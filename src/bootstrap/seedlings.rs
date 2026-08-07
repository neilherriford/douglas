use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use file_system::RelativePathError;
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use seedbank::{Name, NameParseError, SeedlingDefinition};
use seedbank_types::Version;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("Bract error: {0}")]
    BractError(#[from] bract_client::Error),
    #[error("Seedbank error: {0}")]
    SeedbankError(#[from] seedbank_client::Error),
    #[error("Relative path error")]
    RelativePathError(#[from] RelativePathError),
    #[error("Name parse error")]
    NameParseError(#[from] NameParseError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct Context<'a> {
    bract_client: &'a dyn bract_client::Client,
}

#[derive(Default)]
struct State {
    core_seedlings: Vec<(Name, Version, SeedlingDefinition)>,
}

struct StateObserver {
    reporter: Arc<dyn Reporter>,
}

impl StateObserver {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self { reporter }
    }

    pub fn discover(&mut self, span: &Span) -> Result<State, BootstrapError> {
        let guard = span
            .create_child(
                "Loading douglas system, discovering current seedling state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State::default();

        for (name, version, seedling_definition) in core_seedlings::all()? {
            result.core_seedlings.push((
                name.clone(),
                version.clone(),
                seedling_definition.clone(),
            ));
        }

        guard.finish(Ok(result))
    }
}

fn create_plan<'a>(mut state: State) -> Result<Vec<Step<'a>>, BootstrapError> {
    let mut result = Vec::new();

    for (name, version, definition) in state.core_seedlings.drain(std::ops::RangeFull) {
        push_step(
            &mut result,
            ReconcileSeedling::new(name, version, definition),
        );
    }

    Ok(result)
}

pub async fn perform(
    reporter: Arc<dyn Reporter>,
    bract_client: Arc<dyn bract_client::Client>,
) -> bool {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Bootstraping core seedlings",
        log::ScopeKind::Group,
    )
    .start_guard();

    let mut state_observer = StateObserver::new(Arc::clone(&reporter));
    let state = match state_observer.discover(guard.span()) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return false;
        }
    };

    let Ok(plan) = resolve_plan(guard.span(), create_plan(state)) else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return false;
    };

    let mut context = Context {
        bract_client: bract_client.as_ref(),
    };

    if let Ok(()) = execute_plan(guard.span(), plan, &mut context, |_reason| ()).await {
        guard.finish_with_outcome(Outcome::Ok);
        true
    } else {
        guard.finish_with_outcome(Outcome::Failed);
        false
    }
}

struct ReconcileSeedling {
    name: Name,
    version: Version,
    definition: SeedlingDefinition,
}

impl ReconcileSeedling {
    pub fn new(name: Name, version: Version, definition: SeedlingDefinition) -> Self {
        Self {
            name,
            version,
            definition,
        }
    }
}

impl std::fmt::Display for ReconcileSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Reconcile seedling definition '{}' ", self.name)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ReconcileSeedling {
    fn name(&self) -> String {
        "Reconcile Seedling Definition".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Reconcile seedling definition '{}'!", self.name),
                ScopeKind::Step,
            )
            .start_guard();
        context
            .bract_client
            .reconcile_seedling(&self.name, &self.version, &self.definition)
            .await?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub mod core_seedlings {
    use crate::bootstrap::seedlings::BootstrapError;
    use docker_types::VersionedImageName;
    use seedbank::{Mount, MountContents, MountType, Name, SeedlingDefinition};
    use seedbank_types::Version;
    use std::str::FromStr;
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
    };

    pub fn all() -> Result<Vec<(Name, Version, SeedlingDefinition)>, BootstrapError> {
        Ok(vec![traefik()?])
    }

    fn traefik() -> Result<(Name, Version, SeedlingDefinition), BootstrapError> {
        let name = Name::from_str("traefik")?;
        let version = Version(2);
        let mount_name: Name = "config".parse()?;

        let definition = SeedlingDefinition::new(
            VersionedImageName::specific(name.as_ref(), "v3.7.7"),
            HashMap::from([(
                mount_name,
                Mount::build(
                    MountType::Persisted,
                    PathBuf::from("/etc/traefik"),
                    seedbank_types::AccessMode::Writable,
                    HashSet::from([
                        MountContents::file("traefik.yml", generate_default_static_definition())?,
                        MountContents::folder_only("dynamic")?,
                    ]),
                ),
            )]),
        );

        Ok((name, version, definition))
    }

    fn generate_default_static_definition() -> &'static [u8] {
        r#"entryPoints:
  web:
    address: ":80"

providers:
  file:
    directory: "/etc/traefik/dynamic"
    watch: true

log:
  level: INFO
"#
        .as_bytes()
    }
}
