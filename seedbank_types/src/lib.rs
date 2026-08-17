use docker_types::VersionedImageName;
use file_system::{RelativePath, RelativePathError};
use refined_string::{StringRules, Validated};
use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};
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
    pub contents: Vec<u8>,
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
    #[serde(skip)]
    contents: HashSet<MountContents>,
    access_mode: AccessMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessMode {
    ReadOnly,
    Writable,
}

impl Mount {
    pub fn build(
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteSpec {
    Root,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedlingDefinition {
    pub image: VersionedImageName,
    pub mounts: HashMap<Name, Mount>,
    pub routing: Routing,
    pub published_ports: Vec<PortMapping>,
}

impl SeedlingDefinition {
    pub fn new(image: VersionedImageName, mounts: HashMap<Name, Mount>, routing: Routing) -> Self {
        Self {
            image,
            mounts,
            routing,
            published_ports: Vec::new(),
        }
    }

    pub fn with_published_ports(mut self, published_ports: Vec<PortMapping>) -> Self {
        self.published_ports = published_ports;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedlingSpec {
    pub mounts: HashMap<Name, Mount>,
    pub ports: PortSpec,
}

impl SeedlingSpec {
    pub fn new(mounts: HashMap<Name, Mount>, ports: PortSpec) -> Self {
        Self { mounts, ports }
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    Names { names: Vec<Name> },
    Exists { exists: bool },
    Status { status: SeedlingStatus },
    Seedling { seedling: Seedling },
    Ok,
    Error { message: String },
}
