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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealState {
    Uninitialized,
    Sealed,
    Unsealed,
}

impl Status {
    pub fn seal_state(&self) -> SealState {
        if !self.initialized {
            SealState::Uninitialized
        } else if self.sealed {
            SealState::Sealed
        } else {
            SealState::Unsealed
        }
    }
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
    ManagedSecrets,
    PublicKeyInfrastructure,
}

impl Mounts {
    pub fn engine_type(&self) -> &'static str {
        match self {
            Mounts::KeyValueStore | Mounts::ManagedSecrets => "kv",
            Mounts::PublicKeyInfrastructure => "pki",
        }
    }
}

impl std::fmt::Display for Mounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mounts::KeyValueStore => f.write_str("kv"),
            Mounts::ManagedSecrets => f.write_str("douglas"),
            Mounts::PublicKeyInfrastructure => f.write_str("pki"),
        }
    }
}

pub const MANAGED_SECRETS_MAX_VERSIONS: u32 = 20;

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct KvConfig {
    pub max_versions: u32,
    pub cas_required: bool,
}

impl KvConfig {
    pub fn managed_secrets() -> Self {
        Self {
            max_versions: MANAGED_SECRETS_MAX_VERSIONS,
            cas_required: true,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum KvPathError {
    Empty,
    EmptySegment,
    RelativeSegment,
    InvalidCharacter(char),
}

impl std::fmt::Display for KvPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KvPathError::Empty => f.write_str("key value path cannot be empty"),
            KvPathError::EmptySegment => f.write_str("key value path has an empty segment"),
            KvPathError::RelativeSegment => {
                f.write_str("key value path cannot contain '.' or '..' segments")
            }
            KvPathError::InvalidCharacter(character) => {
                write!(f, "key value path contains invalid character {character:?}")
            }
        }
    }
}

impl std::error::Error for KvPathError {}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct KvPath(String);

impl KvPath {
    pub fn new(path: &str) -> Result<Self, KvPathError> {
        if path.is_empty() {
            return Err(KvPathError::Empty);
        }

        for segment in path.split('/') {
            if segment.is_empty() {
                return Err(KvPathError::EmptySegment);
            }
            if segment == "." || segment == ".." {
                return Err(KvPathError::RelativeSegment);
            }
            if let Some(character) = segment
                .chars()
                .find(|character| !Self::is_allowed(*character))
            {
                return Err(KvPathError::InvalidCharacter(character));
            }
        }

        Ok(Self(path.to_string()))
    }

    fn is_allowed(character: char) -> bool {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
    }
}

impl std::str::FromStr for KvPath {
    type Err = KvPathError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Self::new(path)
    }
}

impl std::fmt::Display for KvPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

impl std::fmt::Display for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SecretRecord {
    pub data: std::collections::HashMap<String, SecretValue>,
    pub version: u32,
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
    fn status(initialized: bool, sealed: bool) -> Status {
        Status {
            initialized,
            sealed,
            ..Status::default()
        }
    }

    #[test]
    fn test_seal_state_should_be_uninitialized_before_the_first_initialization() {
        assert_eq!(status(false, true).seal_state(), SealState::Uninitialized);
    }

    #[test]
    fn test_seal_state_should_call_an_uninitialized_instance_uninitialized_even_if_it_claims_to_be_unsealed()
     {
        assert_eq!(status(false, false).seal_state(), SealState::Uninitialized);
    }

    #[test]
    fn test_seal_state_should_be_sealed_when_initialized_and_sealed() {
        assert_eq!(status(true, true).seal_state(), SealState::Sealed);
    }

    #[test]
    fn test_seal_state_should_be_unsealed_when_initialized_and_not_sealed() {
        assert_eq!(status(true, false).seal_state(), SealState::Unsealed);
    }

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
    fn managed_secrets_should_mount_at_douglas() {
        assert_eq!(Mounts::ManagedSecrets.to_string(), "douglas");
        assert_eq!(
            serde_json::to_string(&Mounts::ManagedSecrets).unwrap(),
            r#""douglas""#
        );
    }

    #[test]
    fn engine_type_should_be_kv_for_both_key_value_mounts() {
        assert_eq!(Mounts::KeyValueStore.engine_type(), "kv");
        assert_eq!(Mounts::ManagedSecrets.engine_type(), "kv");
    }

    #[test]
    fn engine_type_should_be_pki_for_the_public_key_infrastructure_mount() {
        assert_eq!(Mounts::PublicKeyInfrastructure.engine_type(), "pki");
    }

    #[test]
    fn kv_path_should_accept_slash_separated_segments_of_safe_characters() {
        assert!(KvPath::new("seedlings/hello-world/DB_PASSWORD").is_ok());
        assert!(KvPath::new("a.b/c_d/e-f").is_ok());
    }

    #[test]
    fn kv_path_should_display_the_path_it_was_built_from() {
        assert_eq!(
            KvPath::new("seedlings/hello/KEY").unwrap().to_string(),
            "seedlings/hello/KEY"
        );
    }

    #[test]
    fn kv_path_should_reject_an_empty_path() {
        assert_eq!(KvPath::new(""), Err(KvPathError::Empty));
    }

    #[test]
    fn kv_path_should_reject_empty_segments() {
        for path in ["/leading", "trailing/", "double//slash"] {
            assert_eq!(KvPath::new(path), Err(KvPathError::EmptySegment), "{path}");
        }
    }

    #[test]
    fn kv_path_should_reject_relative_segments() {
        for path in ["../escape", "a/../b", "./a", "a/."] {
            assert_eq!(
                KvPath::new(path),
                Err(KvPathError::RelativeSegment),
                "{path}"
            );
        }
    }

    #[test]
    fn kv_path_should_reject_characters_that_could_change_the_request() {
        assert_eq!(
            KvPath::new("a?list=true"),
            Err(KvPathError::InvalidCharacter('?'))
        );
        assert_eq!(KvPath::new("a b"), Err(KvPathError::InvalidCharacter(' ')));
        assert_eq!(KvPath::new("a%2e"), Err(KvPathError::InvalidCharacter('%')));
        assert_eq!(
            KvPath::new("a\nb"),
            Err(KvPathError::InvalidCharacter('\n'))
        );
        assert_eq!(
            KvPath::new("a\\b"),
            Err(KvPathError::InvalidCharacter('\\'))
        );
    }

    #[test]
    fn secret_value_should_redact_in_debug_and_display() {
        let value = SecretValue::new("hunter2");

        assert!(!format!("{value:?}").contains("hunter2"));
        assert!(!format!("{value}").contains("hunter2"));
    }

    #[test]
    fn secret_value_should_expose_its_value_only_on_request() {
        assert_eq!(SecretValue::new("hunter2").expose(), "hunter2");
    }

    #[test]
    fn secret_value_should_serialize_as_a_plain_string() {
        assert_eq!(
            serde_json::to_string(&SecretValue::new("hunter2")).unwrap(),
            r#""hunter2""#
        );
    }

    #[test]
    fn secret_value_should_deserialize_from_a_plain_string() {
        let value: SecretValue = serde_json::from_str(r#""hunter2""#).unwrap();

        assert_eq!(value.expose(), "hunter2");
    }

    #[test]
    fn secret_record_should_not_reveal_its_values_in_debug() {
        let record = SecretRecord {
            data: std::collections::HashMap::from([(
                "value".to_string(),
                SecretValue::new("hunter2"),
            )]),
            version: 3,
        };

        assert!(!format!("{record:?}").contains("hunter2"));
    }

    #[test]
    fn kv_config_managed_secrets_should_retain_many_versions_and_require_check_and_set() {
        let config = KvConfig::managed_secrets();

        assert_eq!(config.max_versions, MANAGED_SECRETS_MAX_VERSIONS);
        assert!(config.cas_required);
    }

    #[test]
    fn kv_config_should_deserialize_ignoring_the_fields_it_does_not_model() {
        let config: KvConfig = serde_json::from_str(
            r#"{"max_versions":20,"cas_required":true,"delete_version_after":"0s"}"#,
        )
        .unwrap();

        assert_eq!(config, KvConfig::managed_secrets());
    }

    #[test]
    fn kv_config_should_serialize_with_the_field_names_openbao_expects() {
        assert_eq!(
            serde_json::to_string(&KvConfig::managed_secrets()).unwrap(),
            r#"{"max_versions":20,"cas_required":true}"#
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
