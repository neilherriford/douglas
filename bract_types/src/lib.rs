use seedbank_types::{Name, SeedlingDefinition, SeedlingSpec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
pub struct Orphans {
    pub containers: Vec<Name>,
    pub networks: Vec<Name>,
    pub route_files: Vec<Name>,
    pub resin_repositories: Vec<String>,
}

impl Orphans {
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
            && self.networks.is_empty()
            && self.route_files.is_empty()
            && self.resin_repositories.is_empty()
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
        seedling_spec: SeedlingSpec,
    },
    FindOrphans,
    PruneOrphans {
        orphans: Orphans,
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
    Dropped,
    Orphans(Orphans),
    Pruned,
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
            Response::Orphans(_) => f.write_str("orphans"),
            Response::Pruned => f.write_str("pruned"),
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
