use docker_types::{Capability, VersionedImageName};
use file_system::{RelativePath, RelativePathError};
use refined_string::{StringRules, Validated};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
    num::ParseIntError,
    path::{Path, PathBuf},
    str::FromStr,
    sync::LazyLock,
};
use thiserror::Error;

pub static MAX_SEEDLINGS: u16 = 4096;
pub static RESERVED_SEEDLING_NAMES: &[&str] = &["traefik", "default"];

#[derive(Debug, Error)]
pub enum NameParseError {
    #[error("Name cannot be empty")]
    CannotBeEmpty,
    #[error("Name too long")]
    TooLong,
    #[error("Name is invalid")]
    InvalidName,
}

impl From<refined_string::Error> for NameParseError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => NameParseError::CannotBeEmpty,
            refined_string::Error::TooLong => NameParseError::TooLong,
            refined_string::Error::InvalidName(_) => NameParseError::InvalidName,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SeedlingStatus {
    Defined(Version),
    Unknown,
}

pub struct NameRules;

impl StringRules for NameRules {
    type Error = NameParseError;

    const MAX_LEN: usize = 16;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[._-][a-z0-9]+)*$").unwrap());
        &PATTERN
    }

    fn invalid(_value: &str) -> Self::Error {
        NameParseError::InvalidName
    }
}

pub type Name = Validated<NameRules>;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u16);

impl FromStr for Version {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Version(s.parse()?))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionedName {
    pub name: Name,
    pub version: Version,
}

#[derive(Debug, Error)]
pub enum IdParseError {
    #[error("Id too large {0}")]
    TooLarge(u16),
    #[error("Integer parse error {0}")]
    ParseIntError(#[from] ParseIntError),
}

#[derive(Debug, PartialEq, Eq, Clone, Hash, PartialOrd, Ord)]
pub struct Id {
    pub value: u16,
}

impl Id {
    pub fn assert_is_valid(value: u16) -> Result<(), IdParseError> {
        if value >= MAX_SEEDLINGS {
            Err(IdParseError::TooLarge(value))
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value.to_string())
    }
}

impl FromStr for Id {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value: u16 = s.parse()?;
        Self::assert_is_valid(value)?;
        Ok(Self { value })
    }
}

impl Serialize for Id {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.value.to_string())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Id::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Seedling {
    pub id: Id,
    pub name: Name,
    pub version: Version,
    pub definition: SeedlingDefinition,
}

impl std::fmt::Display for Seedling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name.as_ref())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum MountType {
    Persisted,
    PersistedShared(Vec<Name>),
    InMemory,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MountFile {
    pub file_relative_path: RelativePath,
    #[serde(with = "contents_as_string")]
    pub contents: Vec<u8>,
}

mod contents_as_string {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(contents: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        std::str::from_utf8(contents)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(String::deserialize(deserializer)?.into_bytes())
    }
}

impl MountFile {
    pub fn in_root(contents: Vec<u8>) -> Self {
        Self::new(RelativePath::root(), contents)
    }

    pub fn new(file_relative_path: RelativePath, contents: Vec<u8>) -> Self {
        Self {
            file_relative_path,
            contents,
        }
    }
}

impl std::hash::Hash for MountFile {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.file_relative_path.hash(state);
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum MountContents {
    FolderOnly(RelativePath),
    File(MountFile),
}

impl MountContents {
    pub fn file(
        relative_file_path: &str,
        contents: &[u8],
    ) -> Result<MountContents, RelativePathError> {
        let file_relative_path = RelativePath::try_from(PathBuf::from(relative_file_path))?;
        Ok(MountContents::File(MountFile {
            file_relative_path,
            contents: Vec::from(contents),
        }))
    }

    pub fn folder_only(relative_path: &str) -> Result<MountContents, RelativePathError> {
        let relative_path = RelativePath::try_from(PathBuf::from(relative_path))?;
        Ok(MountContents::FolderOnly(relative_path))
    }

    pub fn relative_path(&self) -> &RelativePath {
        match self {
            MountContents::FolderOnly(path) => path,
            MountContents::File(file) => &file.file_relative_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    kind: MountType,
    remote_path: PathBuf,
    #[serde(default)]
    contents: HashSet<MountContents>,
    access_mode: AccessMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessMode {
    ReadOnly,
    Writable,
}

impl Mount {
    pub fn with_files(
        kind: MountType,
        remote_path: PathBuf,
        access_mode: AccessMode,
        contents: HashSet<MountContents>,
    ) -> Self {
        Self {
            kind,
            remote_path,
            access_mode,
            contents,
        }
    }

    pub fn empty(kind: MountType, remote_path: PathBuf, access_mode: AccessMode) -> Self {
        Self::with_files(kind, remote_path, access_mode, HashSet::new())
    }

    pub fn contents(&self) -> &HashSet<MountContents> {
        &self.contents
    }

    pub fn remote_path(&self) -> &Path {
        &self.remote_path
    }

    pub fn kind(&self) -> &MountType {
        &self.kind
    }

    pub fn contents_mut(&mut self) -> &mut HashSet<MountContents> {
        &mut self.contents
    }

    pub fn access_mode(&self) -> &AccessMode {
        &self.access_mode
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSpec {
    #[default]
    Root,
    Subdomain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub external: u16,
    pub internal: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortSpec {
    pub public: u16,
    pub additional: Vec<PortMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Routing {
    None,
    Routed { route: RouteSpec, ports: PortSpec },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretsAccess {
    pub read: bool,
    pub write: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Core,
    #[default]
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCheckCommand(String);

#[derive(Debug, Error)]
#[error("command cannot be empty")]
pub struct HealthCheckCommandParseError;

impl FromStr for HealthCheckCommand {
    type Err = HealthCheckCommandParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            Err(HealthCheckCommandParseError)
        } else {
            Ok(Self(s.to_string()))
        }
    }
}

impl Serialize for HealthCheckCommand {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HealthCheckCommand {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for HealthCheckCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub command: HealthCheckCommand,
    pub wait_time_in_seconds: std::num::NonZeroU8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedlingDefinition {
    pub image: VersionedImageName,
    pub mounts: HashMap<Name, Mount>,
    pub routing: Routing,
    pub published_ports: Vec<PortMapping>,
    pub command: Option<String>,
    pub added_capabilities: HashSet<Capability>,
    #[serde(default)]
    pub secrets: Option<SecretsAccess>,
    #[serde(default)]
    pub origin: Origin,
    pub health_check: HealthCheck,
}

impl SeedlingDefinition {
    pub fn new(
        image: VersionedImageName,
        mounts: HashMap<Name, Mount>,
        routing: Routing,
        health_check: HealthCheck,
    ) -> Self {
        Self {
            image,
            mounts,
            routing,
            published_ports: Vec::new(),
            command: None,
            added_capabilities: HashSet::new(),
            secrets: None,
            origin: Origin::default(),
            health_check,
        }
    }

    pub fn with_published_ports(mut self, published_ports: Vec<PortMapping>) -> Self {
        self.published_ports = published_ports;
        self
    }

    pub fn with_command(mut self, command: &str) -> Self {
        self.command = Some(command.to_string());
        self
    }

    pub fn with_capability(mut self, capability: Capability) -> Self {
        self.added_capabilities.insert(capability);
        self
    }

    pub fn with_secrets_access(mut self, secrets: Option<SecretsAccess>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSeedlingDefinition {
    pub mounts: HashMap<Name, Mount>,
    pub ports: PortSpec,
    #[serde(default)]
    pub route: RouteSpec,
    #[serde(default)]
    pub secrets: Option<SecretsAccess>,
    pub health_check: HealthCheck,
}

impl UserSeedlingDefinition {
    pub fn new(mounts: HashMap<Name, Mount>, ports: PortSpec, health_check: HealthCheck) -> Self {
        Self {
            mounts,
            ports,
            route: RouteSpec::default(),
            secrets: None,
            health_check,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    List,
    Exists {
        name: Name,
    },
    Status {
        name: Name,
    },
    Load {
        name: Name,
    },
    Create {
        name: Name,
        version: Version,
        definition: SeedlingDefinition,
    },
    Delete {
        name: Name,
    },
    Update {
        name: Name,
        version: Version,
        definition: SeedlingDefinition,
    },
    Default,
    ClaimDefault {
        name: Name,
    },
    ReleaseDefault {
        name: Name,
    },
    GetDesiredRunStatus {
        name: Name,
    },
    SetDesiredRunStatus {
        name: Name,
        desired_run_status: DesiredRunStatus,
    },
    ResetHealthLog {
        name: Name,
    },
    HealthCheckLog {
        name: Name,
    },
    IncrementHealthLogFailCount {
        name: Name,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    Names { names: Vec<Name> },
    Exists { exists: bool },
    Status { status: SeedlingStatus },
    Seedling { seedling: Box<Seedling> },
    Default { name: Option<Name> },
    Ok,
    Error { message: String },
    DesiredRunStatus { desired_run_status: DesiredRunStatus },
    HealthCheckLog { log: Option<HealthCheckLog> },
    IncrementHealthLogFailCount { reached_max_fail_count: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesiredRunStatus {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckLog {
    pub fail_count: u8,
    pub updated_at: std::time::SystemTime,
}

impl Default for HealthCheckLog {
    fn default() -> Self {
        Self {
            fail_count: 1,
            updated_at: std::time::SystemTime::now(),
        }
    }
}

impl HealthCheckLog {
    pub fn reached_max_fail_count(&self) -> bool {
        self.fail_count > 5
    }

    pub fn increment(&mut self) {
        self.fail_count += 1;
        self.updated_at = std::time::SystemTime::now();
    }
}

#[cfg(test)]
mod tests {

    use std::num::NonZeroU8;

    use super::*;

    #[test]
    fn test_response_desired_run_status_should_serialize() {
        let response = Response::DesiredRunStatus {
            desired_run_status: DesiredRunStatus::Running,
        };

        serde_json::to_string(&response).expect("should serialize");
    }

    #[test]
    fn test_response_increment_health_log_fail_count_should_serialize() {
        let response = Response::IncrementHealthLogFailCount {
            reached_max_fail_count: true,
        };

        serde_json::to_string(&response).expect("should serialize");
    }

    #[test]
    fn test_response_health_check_log_should_serialize_when_some() {
        let response = Response::HealthCheckLog {
            log: Some(HealthCheckLog::default()),
        };

        serde_json::to_string(&response).expect("should serialize");
    }

    #[test]
    fn test_response_health_check_log_should_serialize_when_none() {
        let response = Response::HealthCheckLog { log: None };

        serde_json::to_string(&response).expect("should serialize");
    }

    #[test]
    fn test_mount_file_contents_should_round_trip_through_toml_as_text() {
        let mount = Mount::with_files(
            MountType::Persisted,
            PathBuf::from("/usr/src/app/public"),
            AccessMode::ReadOnly,
            HashSet::from([
                MountContents::file("index.html", b"<h1>Hello, world!</h1>\n")
                    .expect("valid relative path"),
            ]),
        );

        let toml = toml::to_string_pretty(&mount).expect("should serialize");
        assert!(toml.contains("<h1>Hello, world!</h1>"));

        let round_tripped: Mount = toml::from_str(&toml).expect("should deserialize");
        assert_eq!(round_tripped, mount);
    }

    #[test]
    fn test_mount_contents_should_default_to_empty_when_omitted() {
        let toml = r#"
            kind = "Persisted"
            remote_path = "/etc/example/config"
            access_mode = "ReadOnly"
        "#;

        let mount: Mount = toml::from_str(toml).expect("should deserialize");

        assert!(mount.contents().is_empty());
    }

    fn seedling_definition() -> SeedlingDefinition {
        SeedlingDefinition::new(
            docker_types::VersionedImageName::specific("hello-world", "1"),
            HashMap::new(),
            Routing::None,
            HealthCheck {
                command: HealthCheckCommand::from_str("true").unwrap(),
                wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
            },
        )
    }

    #[test]
    fn test_origin_should_default_to_user() {
        assert_eq!(Origin::default(), Origin::User);
    }

    #[test]
    fn test_seedling_definition_new_should_default_origin_to_user() {
        assert_eq!(seedling_definition().origin, Origin::User);
    }

    #[test]
    fn test_seedling_definition_should_carry_the_origin_set_via_with_origin() {
        let definition = seedling_definition().with_origin(Origin::Core);

        assert_eq!(definition.origin, Origin::Core);
    }

    #[test]
    fn test_seedling_definition_should_default_origin_to_user_when_omitted_from_serialized_toml() {
        let toml = toml::to_string(&seedling_definition().with_origin(Origin::Core))
            .expect("should serialize");
        let toml_without_origin: String = toml
            .lines()
            .filter(|line| !line.starts_with("origin"))
            .collect::<Vec<_>>()
            .join("\n");

        let definition: SeedlingDefinition =
            toml::from_str(&toml_without_origin).expect("should deserialize");

        assert_eq!(definition.origin, Origin::User);
    }
}
