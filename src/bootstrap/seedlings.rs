use async_trait::async_trait;
use blueprint::Command;
use config::DouglasFolders;
use file_system::RelativePathError;
use log::{Outcome, Reporter, ScopeKind, Span};
use seedbank::{Name, NameParseError, SeedlingDefinition};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("Bract error: {0}")]
    BractError(#[from] bract_client::Error),
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
    core_seedlings_missing: Vec<(Name, SeedlingDefinition)>,
    core_seedlings_needing_start: Vec<Name>,
}

struct StateObserver<'a> {
    reporter: Arc<dyn Reporter>,
    bract_client: &'a dyn bract_client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(reporter: Arc<dyn Reporter>, bract_client: &'a dyn bract_client::Client) -> Self {
        Self {
            reporter,
            bract_client,
        }
    }

    pub async fn discover(&mut self, span: &Span) -> Result<State, BootstrapError> {
        todo!()
        // let guard = span
        //     .create_child(
        //         "Loading douglas system, discovering current seedling state",
        //         ScopeKind::Phase,
        //     )
        //     .start_guard();

        // let mut result = State::default();

        // match self
        //     .bract_client
        //     .status(&core_seedling_definitions::TRAEFIK)
        //     .await?
        // {
        //     bract::SeedlingStatus::Running => (),
        //     bract::SeedlingStatus::Defined => result
        //         .core_seedlings_needing_start
        //         .push(core_seedling_definitions::TRAEFIK.clone()),
        //     bract::SeedlingStatus::Unknown => {
        //         result.core_seedlings_missing.push((
        //             core_seedling_definitions::TRAEFIK.clone(),
        //             core_seedling_definitions::traefik()?,
        //         ));
        //         result
        //             .core_seedlings_needing_start
        //             .push(core_seedling_definitions::TRAEFIK.clone());
        //     }
        // }

        // guard.finish_with_outcome(log::Outcome::Ok);
        // Ok(result)
    }
}

fn create_plan<'a>(mut state: State) -> Result<Vec<Step<'a>>, BootstrapError> {
    let mut result = Vec::new();

    for (name, seedling) in state.core_seedlings_missing.drain(std::ops::RangeFull) {
        push_step(&mut result, CreateSeedling::new(name, seedling));
    }

    todo!();
}

pub async fn perform(reporter: Arc<dyn Reporter>) {}

struct CreateSeedling {
    name: Name,
    seedling: SeedlingDefinition,
}

impl CreateSeedling {
    pub fn new(name: Name, seedling: SeedlingDefinition) -> Self {
        Self { name, seedling }
    }
}

impl std::fmt::Display for CreateSeedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create seedling '{}' ", self.name)
    }
}

#[async_trait(?Send)]
impl<'a> Command<Context<'a>> for CreateSeedling {
    fn name(&self) -> String {
        "Create Seedling".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guard = span
            .create_child(
                &format!("Created seedling '{}'!", self.name),
                ScopeKind::Step,
            )
            .start_guard();
        context
            .bract_client
            .create_seedling(&self.name, &self.seedling)
            .await?;
        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub mod core_seedling_definitions {
    use crate::bootstrap::seedlings::BootstrapError;
    use docker_types::VersionedImageName;
    use seedbank::{Mount, MountContents, MountType, Name, SeedlingDefinition};
    use std::str::FromStr;
    use std::sync::LazyLock;
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
    };

    #[allow(
        clippy::expect_used,
        reason = "TRAEFIK is a hardcoded literal, known valid at compile time"
    )]
    pub static TRAEFIK: LazyLock<Name> =
        LazyLock::new(|| Name::from_str("traefik").expect("valid name"));

    pub fn traefik() -> Result<SeedlingDefinition, BootstrapError> {
        let mount_name: Name = "config".parse()?;

        Ok(SeedlingDefinition::new(
            VersionedImageName::specific(TRAEFIK.as_ref(), "v3.7.7"),
            HashMap::from([(
                mount_name,
                Mount::build(
                    MountType::Persisted,
                    PathBuf::from("/etc/traefik"),
                    HashSet::from([
                        MountContents::file("traefik.yml", generate_default_static_definition())?,
                        MountContents::folder_only("dynamic")?,
                    ]),
                ),
            )]),
        ))
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
