use seedbank_types::{Name, SeedlingDefinition, UserSeedlingDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OpenBaoReport {
    pub is_running: bool,
    pub is_initialized: bool,
    pub is_sealed: bool,
    pub credentials_available: bool,
    pub credentials_work: bool,
    pub mounts: HashMap<String, String>,
    pub app_role_enabled: bool,
    pub acme_enabled: bool,
    pub root_ca_configured: bool,
    pub acme_pki_role_created: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mount {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum SeedlingStatus {
    Running(DefinitionStatus),
    Defined(DefinitionStatus),
    Missing,
    Unknown,
}

impl std::fmt::Display for SeedlingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedlingStatus::Running(definition_status) => {
                f.write_str(&format!("running: {definition_status}"))
            }
            SeedlingStatus::Defined(definition_status) => {
                f.write_str(&format!("defined: {definition_status}"))
            }
            SeedlingStatus::Missing => f.write_str("missing"),
            SeedlingStatus::Unknown => f.write_str("unknown seedling"),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deadwood {
    pub containers: Vec<Name>,
    pub networks: Vec<Name>,
    pub route_files: Vec<Name>,
    pub resin_repositories: Vec<String>,
    pub mounts: Vec<Name>,
    pub openbao_secrets: Vec<Name>,
}

impl Deadwood {
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
            && self.networks.is_empty()
            && self.route_files.is_empty()
            && self.resin_repositories.is_empty()
            && self.mounts.is_empty()
            && self.openbao_secrets.is_empty()
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum DefinitionStatus {
    Current,
    Stale,
    Newer,
}

impl std::fmt::Display for DefinitionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefinitionStatus::Current => f.write_str("current"),
            DefinitionStatus::Stale => f.write_str("stale"),
            DefinitionStatus::Newer => f.write_str("newer"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    SeedlingStatus {
        name: Name,
    },
    StartSeedling {
        name: Name,
    },
    StopSeedling {
        name: Name,
    },
    DropSeedling {
        name: Name,
    },
    ReconcileSeedling {
        name: Name,
        version: seedbank_types::Version,
        seedling_definition: SeedlingDefinition,
    },
    NewSeedling {
        name: Name,
        user_seedling_definition: UserSeedlingDefinition,
    },
    FindDeadwood,
    PruneDeadwood {
        deadwood: Deadwood,
    },
    ListSeedlings,
    OpenBaoStatus,
    StopBract {
        including_containers: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    SeedlingStatus(SeedlingStatus),
    Created { message: String },
    Updated,
    Started,
    Stopped,
    BractStopped { including_containers: bool },
    Dropped,
    Deadwood(Deadwood),
    Pruned,
    Seedlings { names: Vec<Name> },
    OpenBaoStatus(OpenBaoReport),
    Error { message: String },
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Response::SeedlingStatus(seedling_status) => {
                f.write_str(&format!("status: '{seedling_status}'"))
            }
            Response::Created { .. } => f.write_str("created"),
            Response::Updated => f.write_str("updated"),
            Response::Started => f.write_str("started"),
            Response::Stopped => f.write_str("stopped"),

            Response::Dropped => f.write_str("dropped"),
            Response::Deadwood(_) => f.write_str("deadwood"),
            Response::Pruned => f.write_str("pruned"),
            Response::Seedlings { names } => f.write_str(&format!("{} seedling(s)", names.len())),
            Response::OpenBaoStatus(_) => f.write_str("openbao status"),
            Response::BractStopped {
                including_containers,
            } => {
                if *including_containers {
                    f.write_str("bract stopped (including containers)")
                } else {
                    f.write_str("bract stopped (excluding containers)")
                }
            }

            Response::Error { message } => f.write_str(&format!("error: '{message}'")),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    Event(log::Event),
    Response(Response),
}

const CONTAINER_NAME_PREFIX: &str = "doug.";
pub fn seedling_name_from_doug_prefixed(raw: &str) -> Option<seedbank_types::Name> {
    raw.strip_prefix(CONTAINER_NAME_PREFIX)
        .and_then(|name| name.parse().ok())
}

const AGENT_CONTAINER_NAME_PREFIX: &str = "doug-agent.";
pub fn agent_container_name(
    seedling_name: &seedbank_types::Name,
) -> Result<docker_types::ContainerName, docker_types::DockerNameError> {
    format!("{AGENT_CONTAINER_NAME_PREFIX}{}", seedling_name.as_ref()).parse()
}

pub fn seedling_name_from_agent_prefixed(raw: &str) -> Option<seedbank_types::Name> {
    raw.strip_prefix(AGENT_CONTAINER_NAME_PREFIX)
        .and_then(|name| name.parse().ok())
}

pub fn container_name(
    seedling_name: &seedbank_types::Name,
) -> Result<docker_types::ContainerName, docker_types::DockerNameError> {
    format!(
        "{prefix}{name}",
        prefix = CONTAINER_NAME_PREFIX,
        name = seedling_name.as_ref()
    )
    .parse()
}

pub fn is_douglas_container(name: &docker_types::ContainerName) -> bool {
    let name = name.to_string();
    name.starts_with(CONTAINER_NAME_PREFIX) || name.starts_with(AGENT_CONTAINER_NAME_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::{EventKind, Level, ScopeId};
    use serde_json::json;

    #[test]
    fn test_response_message_should_not_collide_with_the_inner_responses_own_tag() {
        let message = ServerMessage::Response(Response::Started);

        let value = serde_json::to_value(&message).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "Response",
                "data": { "type": "Started" }
            })
        );
    }

    #[test]
    fn test_response_message_should_round_trip() {
        let message = ServerMessage::Response(Response::Error {
            message: "boom".to_string(),
        });

        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: ServerMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            ServerMessage::Response(Response::Error { message }) => assert_eq!(message, "boom"),
            other => panic!("expected an Error response, got {other:?}"),
        }
    }

    #[test]
    fn test_created_response_should_round_trip() {
        let message = ServerMessage::Response(Response::Created {
            message: "Ready. Push your image to localhost:7376/hello-world to deploy it."
                .to_string(),
        });

        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: ServerMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            ServerMessage::Response(Response::Created { message }) => {
                assert_eq!(
                    message,
                    "Ready. Push your image to localhost:7376/hello-world to deploy it."
                );
            }
            other => panic!("expected a Created response, got {other:?}"),
        }
    }

    #[test]
    fn test_seedlings_response_should_round_trip() {
        let message = ServerMessage::Response(Response::Seedlings {
            names: vec!["hello-world".parse().unwrap()],
        });

        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: ServerMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            ServerMessage::Response(Response::Seedlings { names }) => {
                assert_eq!(names, vec!["hello-world".parse().unwrap()]);
            }
            other => panic!("expected a Seedlings response, got {other:?}"),
        }
    }

    #[test]
    fn test_openbao_status_response_should_round_trip() {
        let report = OpenBaoReport {
            is_running: true,
            mounts: std::collections::HashMap::from([("kv/".to_string(), "kv".to_string())]),
            ..Default::default()
        };
        let message = ServerMessage::Response(Response::OpenBaoStatus(report));

        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: ServerMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            ServerMessage::Response(Response::OpenBaoStatus(report)) => {
                assert!(report.is_running);
                assert_eq!(report.mounts.get("kv/"), Some(&"kv".to_string()));
            }
            other => panic!("expected an OpenBaoStatus response, got {other:?}"),
        }
    }

    #[test]
    fn test_event_message_should_round_trip() {
        let event = log::Event::new(
            ScopeId::new(),
            EventKind::Message {
                level: Level::Info,
                text: "hello".to_string(),
            },
        );
        let message = ServerMessage::Event(event);

        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: ServerMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            ServerMessage::Event(event) => match event.kind {
                EventKind::Message { text, .. } => assert_eq!(text, "hello"),
                other => panic!("expected a Message event, got {other:?}"),
            },
            other => panic!("expected an Event message, got {other:?}"),
        }
    }
}
