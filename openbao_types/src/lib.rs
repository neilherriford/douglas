use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq)]
pub enum Period {
    Hours(usize),
}

impl Serialize for Period {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Period::Hours(amount) => serializer.serialize_str(&format!("{amount}h")),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Default)]
pub enum ReplicationMode {
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "primary")]
    Primary,
    #[serde(rename = "secondary")]
    Secondary,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Status {
    pub initialized: bool,
    pub sealed: bool,
    pub standby: bool,
    #[serde(default)]
    pub performance_standby: bool,
    pub replication_performance_mode: ReplicationMode,
    pub replication_dr_mode: ReplicationMode,
    pub server_time_utc: u32,
    pub version: String,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            initialized: false,
            sealed: true,
            standby: false,
            performance_standby: false,
            replication_performance_mode: ReplicationMode::default(),
            replication_dr_mode: ReplicationMode::default(),
            server_time_utc: 0,
            version: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Secrets {
    pub secrets: Vec<Secret>,
    pub root_token: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Secret {
    pub key: String,
    pub base64: String,
}

#[derive(Debug, PartialEq, Clone)]
pub enum AuthType {
    AppRole,
}

#[derive(Debug, PartialEq, Clone)]
pub struct RoleId(String);

impl RoleId {
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for RoleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AuthType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl std::fmt::Display for AuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthType::AppRole => f.write_str("approle"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Mounts {
    KeyValueStore,
    PublicKeyInfrastructure,
}

impl std::fmt::Display for Mounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mounts::KeyValueStore => f.write_str("kv"),
            Mounts::PublicKeyInfrastructure => f.write_str("pki"),
        }
    }
}

impl Serialize for Mounts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum Capability {
    Create,
    Read,
    Update,
    Delete,
    List,
    Sudo,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Capability::Create => "create",
            Capability::Read => "read",
            Capability::Update => "update",
            Capability::Delete => "delete",
            Capability::List => "list",
            Capability::Sudo => "sudo",
        })
    }
}

impl Serialize for Capability {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_should_serialize_as_hours_suffixed_with_h() {
        assert_eq!(serde_json::to_string(&Period::Hours(4)).unwrap(), r#""4h""#);
    }

    #[test]
    fn auth_type_should_serialize_using_its_display_form() {
        assert_eq!(AuthType::AppRole.to_string(), "approle");
        assert_eq!(
            serde_json::to_string(&AuthType::AppRole).unwrap(),
            r#""approle""#
        );
    }

    #[test]
    fn mounts_should_serialize_using_their_display_form() {
        assert_eq!(Mounts::KeyValueStore.to_string(), "kv");
        assert_eq!(Mounts::PublicKeyInfrastructure.to_string(), "pki");
        assert_eq!(
            serde_json::to_string(&Mounts::KeyValueStore).unwrap(),
            r#""kv""#
        );
    }

    #[test]
    fn capability_should_serialize_using_its_display_form() {
        for (capability, expected) in [
            (Capability::Create, "create"),
            (Capability::Read, "read"),
            (Capability::Update, "update"),
            (Capability::Delete, "delete"),
            (Capability::List, "list"),
            (Capability::Sudo, "sudo"),
        ] {
            assert_eq!(capability.to_string(), expected);
            assert_eq!(
                serde_json::to_string(&capability).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn role_id_should_display_its_wrapped_value() {
        assert_eq!(RoleId::new("role-1".to_string()).to_string(), "role-1");
    }

    #[test]
    fn status_default_should_be_uninitialized_and_sealed() {
        let status = Status::default();

        assert!(!status.initialized);
        assert!(status.sealed);
    }

    #[test]
    fn replication_mode_should_default_to_unknown() {
        assert_eq!(ReplicationMode::default(), ReplicationMode::Unknown);
    }

    #[test]
    fn status_should_default_performance_standby_when_absent() {
        // OpenBao's /v1/sys/health omits `performance_standby` before the vault is initialized.
        let body = r#"{"initialized":false,"sealed":true,"standby":true,"replication_performance_mode":"unknown","replication_dr_mode":"unknown","server_time_utc":1787896733,"version":"2.6.2"}"#;

        let status = serde_json::from_str::<Status>(body).unwrap();

        assert!(!status.performance_standby);
        assert!(!status.initialized);
        assert!(status.sealed);
    }
}
