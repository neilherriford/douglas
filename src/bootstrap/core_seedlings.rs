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
    Bract(#[from] bract_client::Error),
    #[error("Seedbank error: {0}")]
    Seedbank(#[from] seedbank_client::Error),
    #[error("Relative path error")]
    RelativePath(#[from] RelativePathError),
    #[error("Name parse error")]
    NameParse(#[from] NameParseError),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
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

struct StateObserver {}

impl StateObserver {
    pub fn discover(span: &Span) -> Result<State, BootstrapError> {
        let guard = span
            .create_child(
                "Loading douglas system, discovering current seedling state",
                ScopeKind::Phase,
            )
            .start_guard();

        let mut result = State::default();

        for (name, version, seedling_definition) in definitions::all()? {
            result.core_seedlings.push((
                name.clone(),
                version.clone(),
                seedling_definition.clone(),
            ));
        }

        guard.finish(Ok(result))
    }
}

fn create_plan<'a>(mut state: State) -> Vec<Step<'a>> {
    let mut result = Vec::new();

    for (name, version, definition) in state.core_seedlings.drain(std::ops::RangeFull) {
        push_step(
            &mut result,
            ReconcileSeedling::new(name, version, definition),
        );
    }

    result
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

    let state = match StateObserver::discover(guard.span()) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return false;
        }
    };

    let plan = match resolve_plan::<Context, BootstrapError>(guard.span(), Ok(create_plan(state))) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
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

pub mod definitions {
    use crate::bootstrap::core_seedlings::BootstrapError;
    pub use openbao::NAME as OPENBAO_NAME;
    pub use openbao::SOCKET_MOUNT_NAME as OPENBAO_SOCKET_MOUNT_NAME;
    pub use openbao::SOCKET_NAME as OPENBAO_SOCKET_NAME;
    pub use openbao::TCP_PORT as OPENBAO_TCP_PORT;

    use seedbank::{Name, SeedlingDefinition};
    use seedbank_types::Version;

    pub fn all() -> Result<Vec<(Name, Version, SeedlingDefinition)>, BootstrapError> {
        Ok(vec![traefik::create()?, openbao::create()?])
    }

    mod traefik {
        use crate::bootstrap::core_seedlings::BootstrapError;
        use docker_types::VersionedImageName;
        use seedbank::{Mount, MountContents, MountType, Name, SeedlingDefinition};
        use seedbank_types::{HealthCheckCommand, Version};
        use serde_json::json;
        use std::num::NonZeroU8;
        use std::str::FromStr;
        use std::{
            collections::{HashMap, HashSet},
            path::PathBuf,
        };

        pub fn create() -> Result<(Name, Version, SeedlingDefinition), BootstrapError> {
            let name = Name::from_str("traefik")?;
            let version = Version(1);
            let mount_name: Name = "config".parse()?;

            let definition = SeedlingDefinition::new(
                VersionedImageName::specific(name.as_ref(), "v3.7.7"),
                HashMap::from([(
                    mount_name,
                    Mount::with_files(
                        MountType::Persisted,
                        PathBuf::from("/etc/traefik"),
                        seedbank_types::AccessMode::Writable,
                        HashSet::from([
                            MountContents::file(
                                "traefik.yml",
                                &generate_default_static_definition()?,
                            )?,
                            MountContents::folder_only("dynamic")?,
                        ]),
                    ),
                )]),
                seedbank_types::Routing::None,
                seedbank_types::HealthCheck {
                    command: match HealthCheckCommand::from_str("traefik healthcheck") {
                        Ok(command) => command,
                        _ => panic!("Failed to create traefik health check command"),
                    },
                    #[allow(clippy::unwrap_used)]
                    wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
                },
            )
            .with_published_ports(vec![seedbank_types::PortMapping {
                external: 80,
                internal: 80,
            }])
            .with_capability(docker_types::Capability::Chown)
            .with_origin(seedbank_types::Origin::Core);

            Ok((name, version, definition))
        }

        // Do not add an `api:`/dashboard block here. Traefik's container is attached to
        // every routed seedling's isolated network (so it can reach them), which means
        // anything Traefik listens on is reachable from those seedlings too. Leaving the
        // API/dashboard disabled (the default) keeps the only thing seedlings can reach on
        // that interface to the HTTP router itself — no different from what an external
        // client hitting the domain could already do.
        fn generate_default_static_definition() -> Result<Vec<u8>, serde_json::Error> {
            let config = json!({
                "entryPoints": {
                    "web": {
                        "address": ":80"
                    }
                },
                "providers": {
                    "file": {
                        "directory": "/etc/traefik/dynamic",
                        "watch": true
                    }
                },
                "log": {
                    "level": "INFO"
                },
                "ping": {}
            });

            serde_json::to_vec_pretty(&config)
        }
    }

    mod openbao {
        use crate::bootstrap::core_seedlings::BootstrapError;
        use docker_types::VersionedImageName;
        use seedbank::{Mount, MountContents, MountType, Name, SeedlingDefinition};
        use seedbank_types::{HealthCheckCommand, Version};
        use serde_json::json;
        use std::num::NonZeroU8;
        use std::str::FromStr;
        use std::{
            collections::{HashMap, HashSet},
            path::PathBuf,
        };

        pub use ::openbao::SEEDLING_NAME as NAME;
        pub use ::openbao::SOCKET_MOUNT_NAME;
        pub use ::openbao::SOCKET_NAME;
        pub const LOG_PATH: &str = "/var/log/douglas";
        pub const AUDIT_LOG: &str = "openbao_audit.log";
        pub const SOCKET_PATH: &str = "/run/bract/";
        pub use ::openbao::API_PORT as TCP_PORT;
        const DATA_PATH: &str = "/openbao/data";
        const CLUSTER_ADDRESS: &str = "http://127.0.0.1";
        const UNSEALED_STATUS: &str = "0";
        const SEALED_STATUS: &str = "2";

        pub fn create() -> Result<(Name, Version, SeedlingDefinition), BootstrapError> {
            const CONFIG_PATH: &str = "/openbao/config";
            const CONFIG_FILE_NAME: &str = "config.json";

            let name = Name::from_str(NAME)?;
            let version = Version(1);
            let socket_mount: Name = SOCKET_MOUNT_NAME.parse()?;
            let log_mount: Name = "log".parse()?;
            let config_mount: Name = "config".parse()?;
            let data_mount: Name = "data".parse()?;

            let definition = SeedlingDefinition::new(
                VersionedImageName::namespaced_specific(
                    "openbao",
                    "openbao",
                    openbao::IMAGE_VERSION,
                ),
                HashMap::from([
                    (
                        log_mount,
                        Mount::empty(
                            MountType::Persisted,
                            PathBuf::from(LOG_PATH),
                            seedbank_types::AccessMode::Writable,
                        ),
                    ),
                    (
                        socket_mount,
                        Mount::empty(
                            MountType::Persisted,
                            PathBuf::from(SOCKET_PATH),
                            seedbank_types::AccessMode::Writable,
                        ),
                    ),
                    (
                        config_mount,
                        Mount::with_files(
                            MountType::Persisted,
                            PathBuf::from(CONFIG_PATH),
                            seedbank_types::AccessMode::ReadOnly,
                            HashSet::from([
                                MountContents::file(CONFIG_FILE_NAME, &generate_config()?)?,
                                MountContents::folder_only("dynamic")?,
                            ]),
                        ),
                    ),
                    (
                        data_mount,
                        Mount::empty(
                            MountType::Persisted,
                            PathBuf::from(DATA_PATH),
                            seedbank_types::AccessMode::Writable,
                        ),
                    ),
                ]),
                seedbank_types::Routing::None,
                seedbank_types::HealthCheck {
                    command: create_health_check_command(),
                    #[allow(clippy::unwrap_used)]
                    wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
                },
            )
            .with_command(&format!("server -config={CONFIG_PATH}/{CONFIG_FILE_NAME}"))
            .with_origin(seedbank_types::Origin::Core);

            Ok((name, version, definition))
        }

        fn create_health_check_command() -> HealthCheckCommand {
            let command = format!(
                "BAO_ADDR={CLUSTER_ADDRESS}:{TCP_PORT} bao status >/dev/null 2>&1; code=$?; test \"$code\" -eq {UNSEALED_STATUS} -o \"$code\" -eq {SEALED_STATUS}"
            );

            match HealthCheckCommand::from_str(&command) {
                Ok(command) => command,
                _ => panic!("Failed to create OpenBao health check command"),
            }
        }

        fn generate_config() -> Result<Vec<u8>, serde_json::Error> {
            let socket_address = format!("{SOCKET_PATH}{SOCKET_NAME}");
            let api_addr = format!("unix://{socket_address}");

            let config = json!({
                "ui": false,
                "log_level": "info",
                "storage": {
                    "file": {
                        "path": DATA_PATH
                    }
                },
                "audit": [
                    {
                        "file": {
                            "file-log": {
                                "description": "Primary file audit log",
                                "options": {
                                    "file_path": format!("{LOG_PATH}/{AUDIT_LOG}"),
                                    "mode": "0600"
                                }
                            }
                        }
                    }
                ],
                "listener": [
                    { "unix": { "address": socket_address } },
                    { "tcp": { "address": format!("0.0.0.0:{TCP_PORT}"), "tls_disable": true } }
                ],
                "api_addr": api_addr,
                "cluster_addr": format!("{CLUSTER_ADDRESS}:{TCP_PORT}")
            });

            serde_json::to_vec_pretty(&config)
        }
    }
}
