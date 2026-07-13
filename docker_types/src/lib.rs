use refined_string::{StringRules, Validated};
use regex::Regex;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::Value as Json;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImagePathComponentError {
    #[error("image path component cannot be empty")]
    CannotBeEmpty,
    #[error("image path component is too long")]
    TooLong,
    #[error("invalid image path component '{0}'")]
    Invalid(String),
}

impl From<refined_string::Error> for ImagePathComponentError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => ImagePathComponentError::CannotBeEmpty,
            refined_string::Error::TooLong => ImagePathComponentError::TooLong,
            refined_string::Error::InvalidName(value) => ImagePathComponentError::Invalid(value),
        }
    }
}

pub struct ImagePathComponentRules;

impl StringRules for ImagePathComponentRules {
    // Docker doesn't document a hard limit for an individual path component; this
    // is a generous ceiling relative to the ~255 char limit on a whole reference.
    const MAX_LEN: usize = 255;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:(?:\.|_{1,2}|-+)[a-z0-9]+)*$").unwrap());
        &PATTERN
    }

    type Error = ImagePathComponentError;

    fn invalid(value: &str) -> Self::Error {
        ImagePathComponentError::Invalid(value.to_string())
    }
}

pub type ImagePathComponent = Validated<ImagePathComponentRules>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionTagError {
    #[error("tag cannot be empty")]
    CannotBeEmpty,
    #[error("tag is too long")]
    TooLong,
    #[error("invalid tag '{0}'")]
    Invalid(String),
}

impl From<refined_string::Error> for VersionTagError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => VersionTagError::CannotBeEmpty,
            refined_string::Error::TooLong => VersionTagError::TooLong,
            refined_string::Error::InvalidName(value) => VersionTagError::Invalid(value),
        }
    }
}

pub struct VersionTagRules;

impl StringRules for VersionTagRules {
    const MAX_LEN: usize = 128;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9._-]*$").unwrap());
        &PATTERN
    }

    type Error = VersionTagError;

    fn invalid(value: &str) -> Self::Error {
        VersionTagError::Invalid(value.to_string())
    }
}

pub type VersionTag = Validated<VersionTagRules>;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Version {
    Latest,
    Specific(VersionTag),
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Version::Latest => f.write_str("latest"),
            Version::Specific(version) => f.write_str(version.as_ref()),
        }
    }
}

impl FromStr for Version {
    type Err = VersionTagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s == "latest" {
            Ok(Version::Latest)
        } else {
            Ok(Version::Specific(VersionTag::from_str(s)?))
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionedImageNameParseError {
    #[error("expected namespace/image:version or image:version, got '{0}'")]
    Invalid(String),
    #[error("image name cannot be empty")]
    EmptyName,
    #[error("invalid namespace: {0}")]
    InvalidNamespace(String),
    #[error("invalid image name: {0}")]
    InvalidName(String),
    #[error("invalid version: {0}")]
    InvalidVersion(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct VersionedImageName {
    pub namespace: Option<ImagePathComponent>,
    pub name: ImagePathComponent,
    pub version: Version,
}

impl VersionedImageName {
    pub fn latest(name: &str) -> Self {
        Self {
            namespace: None,
            name: ImagePathComponent::from_str(name).expect("valid image name"),
            version: Version::Latest,
        }
    }
    pub fn specific(name: &str, version: &str) -> Self {
        Self {
            namespace: None,
            name: ImagePathComponent::from_str(name).expect("valid image name"),
            version: Version::Specific(VersionTag::from_str(version).expect("valid version tag")),
        }
    }

    pub fn namespaced_latest(namespace: &str, name: &str) -> Self {
        Self {
            namespace: Some(ImagePathComponent::from_str(namespace).expect("valid namespace")),
            name: ImagePathComponent::from_str(name).expect("valid image name"),
            version: Version::Latest,
        }
    }

    pub fn namespaced_specific(namespace: &str, name: &str, version: &str) -> Self {
        Self {
            namespace: Some(ImagePathComponent::from_str(namespace).expect("valid namespace")),
            name: ImagePathComponent::from_str(name).expect("valid image name"),
            version: Version::Specific(VersionTag::from_str(version).expect("valid version tag")),
        }
    }

    pub fn formatted_name(&self) -> String {
        if let Some(namespace) = &self.namespace {
            format!("{namespace}/{}", self.name)
        } else {
            self.name.to_string()
        }
    }

    pub fn version_formatted_name(&self) -> String {
        format!("{}:{}", self.formatted_name(), self.version)
    }
}

impl std::fmt::Display for VersionedImageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.version_formatted_name())
    }
}

impl FromStr for VersionedImageName {
    type Err = VersionedImageNameParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (namespace, name_and_version) = text.split_once('/').unwrap_or(("", text));
        let (name, raw_version) = name_and_version
            .split_once(':')
            .ok_or_else(|| VersionedImageNameParseError::Invalid(text.to_string()))?;

        let namespace = namespace.trim();
        let name = name.trim();
        if name.is_empty() {
            return Err(VersionedImageNameParseError::EmptyName);
        }

        let namespace =
            if namespace.is_empty() {
                None
            } else {
                Some(ImagePathComponent::from_str(namespace).map_err(|err| {
                    VersionedImageNameParseError::InvalidNamespace(err.to_string())
                })?)
            };

        let name = ImagePathComponent::from_str(name)
            .map_err(|err| VersionedImageNameParseError::InvalidName(err.to_string()))?;

        let version = raw_version
            .parse::<Version>()
            .map_err(|err| VersionedImageNameParseError::InvalidVersion(err.to_string()))?;

        Ok(VersionedImageName {
            namespace,
            name,
            version,
        })
    }
}

impl Serialize for VersionedImageName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.version_formatted_name())
    }
}

impl<'de> Deserialize<'de> for VersionedImageName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Capability {
    IpcLock,
    Chown,
}

pub fn serialize_capabilities<S>(
    capabilities: &Vec<Capability>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(capabilities.len()))?;
    for capability in capabilities {
        let text = match capability {
            Capability::IpcLock => "IPC_LOCK",
            Capability::Chown => "CAP_CHOWN",
        };

        seq.serialize_element(&text)?;
    }
    seq.end()
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Id {
    pub algorithm: String,
    pub hex: String,
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

pub fn deserialize_id<'de, D>(deserializer: D) -> Result<Id, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_id: String = Deserialize::deserialize(deserializer)?;
    let parts: Vec<&str> = raw_id.split(':').collect();
    let (algorithm, hex) = match parts.as_slice() {
        [first, second] => (first.to_string(), second.to_string()),
        _ => ("missing-algorithim".to_string(), raw_id),
    };

    Ok(Id { algorithm, hex })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LabelKeyError {
    #[error("label key cannot be empty")]
    CannotBeEmpty,
    #[error("label key is too long")]
    TooLong,
    #[error("invalid label key '{0}'")]
    Invalid(String),
}

impl From<refined_string::Error> for LabelKeyError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => LabelKeyError::CannotBeEmpty,
            refined_string::Error::TooLong => LabelKeyError::TooLong,
            refined_string::Error::InvalidName(value) => LabelKeyError::Invalid(value),
        }
    }
}

pub struct LabelKeyRules;

impl StringRules for LabelKeyRules {
    const MAX_LEN: usize = 255;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:(?:\.|_{1,2}|-+)[a-z0-9]+)*$").unwrap());
        &PATTERN
    }

    type Error = LabelKeyError;

    fn invalid(value: &str) -> Self::Error {
        LabelKeyError::Invalid(value.to_string())
    }
}

pub type LabelKey = Validated<LabelKeyRules>;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Label {
    pub name: LabelKey,
    pub value: String,
}

impl Label {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: LabelKey::from_str(name).expect("valid label key"),
            value: value.to_string(),
        }
    }
}

pub fn deserialize_labels<'de, D>(deserializer: D) -> Result<Vec<Label>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Lables to be an object"))?;

    let result = obj
        .iter()
        .map(|(name, value)| {
            let Some(value) = value.as_str() else {
                return Err(serde::de::Error::custom("Expected value to be a string"));
            };

            Ok(Label {
                name: LabelKey::from_str(name).map_err(serde::de::Error::custom)?,
                value: value.to_string(),
            })
        })
        .collect::<Result<_, D::Error>>()?;

    Ok(result)
}

pub fn serialize_labels<S>(labels: &Vec<Label>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(labels.len()))?;
    for label in labels {
        map.serialize_entry(&label.name, &label.value)?;
    }
    map.end()
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct Tag {
    pub name: String,
    pub version: String,
}

#[derive(Debug, PartialEq, Ord, PartialOrd, Eq, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

impl EnvironmentVariable {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

pub fn deserialize_environment_variables<'de, D>(
    deserializer: D,
) -> Result<Vec<EnvironmentVariable>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_array()
        .ok_or_else(|| serde::de::Error::custom("Expected Env to be an array"))?;

    let result: Vec<EnvironmentVariable> = obj
        .iter()
        .map(|item| {
            if let Some(assignment) = item.as_str() {
                let parts: Vec<&str> = assignment.split('=').collect();
                let (name, value) = match parts.as_slice() {
                    [first, second] => (first.to_string(), second.to_string()),
                    _ => (assignment.to_string(), String::new()),
                };
                Ok(EnvironmentVariable { name, value })
            } else {
                Err(serde::de::Error::custom(
                    "Expected assignments to be strings",
                ))
            }
        })
        .collect::<Result<_, D::Error>>()?;
    Ok(result)
}

pub fn serialize_environment_variables<S>(
    environment_variables: &Vec<EnvironmentVariable>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(environment_variables.len()))?;
    for environment_variable in environment_variables {
        let s = format!(
            "{}={}",
            environment_variable.name, environment_variable.value
        );
        seq.serialize_element(&s)?;
    }
    seq.end()
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    Bind,
    Volume,
    Image,
    Tmpfs,
    Npipe,
    Cluster,
}

impl std::fmt::Display for MountType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let text = match self {
            MountType::Bind => "bind",
            MountType::Volume => "volume",
            MountType::Image => "image",
            MountType::Tmpfs => "tmpfs",
            MountType::Npipe => "npipe",
            MountType::Cluster => "cluster",
        };
        write!(f, "{text}")
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Mount {
    #[serde(rename = "Type")]
    pub mount_type: MountType,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Destination")]
    pub destination: String,
    #[serde(rename = "RW")]
    pub writable: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let text = match self {
            Status::Created => "created",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Restarting => "restarting",
            Status::Removing => "removing",
            Status::Exited => "exited",
            Status::Dead => "dead",
        };
        write!(f, "{text}")
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_str().unwrap_or("<Invalid path>").to_string()
}

#[derive(Debug, PartialEq, Clone)]
pub struct MountDefinition {
    pub name: MountName,
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub writable: bool,
}

impl Serialize for MountDefinition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("VolumeMount", 8)?;

        state.serialize_field("Type", "bind")?;
        state.serialize_field("Name", &self.name)?;
        state.serialize_field("Source", &path_to_string(&self.host_path))?;
        state.serialize_field("Target", &path_to_string(&self.container_path))?;
        state.serialize_field("RW", &self.writable)?;

        state.end()
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ImageIdentifier {
    Id(Id),
    ImageName(VersionedImageName),
}

pub fn serialize_image_identifier<S>(
    image_identifier: &ImageIdentifier,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let id = match image_identifier {
        ImageIdentifier::Id(id) => id.to_string(),
        ImageIdentifier::ImageName(image_name) => image_name.to_string(),
    };
    serializer.collect_str(&id)
}

#[derive(Debug, PartialEq, Clone)]
pub struct ContainerUser {
    pub user_id: u32,
    pub group_id: u32,
}

impl Serialize for ContainerUser {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&format!("{}:{}", self.user_id, self.group_id))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DockerNameError {
    #[error("name cannot be empty")]
    CannotBeEmpty,
    #[error("name is too long")]
    TooLong,
    #[error("invalid name '{0}'")]
    Invalid(String),
}

impl From<refined_string::Error> for DockerNameError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => DockerNameError::CannotBeEmpty,
            refined_string::Error::TooLong => DockerNameError::TooLong,
            refined_string::Error::InvalidName(value) => DockerNameError::Invalid(value),
        }
    }
}

pub struct DockerNameRules;

impl StringRules for DockerNameRules {
    // Docker doesn't document a hard limit; this is a generous ceiling matching
    // the same choice made for image path components.
    const MAX_LEN: usize = 255;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]*$").unwrap());
        &PATTERN
    }

    type Error = DockerNameError;

    fn invalid(value: &str) -> Self::Error {
        DockerNameError::Invalid(value.to_string())
    }
}

pub type ContainerName = Validated<DockerNameRules>;
pub type MountName = Validated<DockerNameRules>;

#[derive(Debug, PartialEq, Clone)]
pub struct ContainerDefinition {
    pub name: ContainerName,
    pub run_as: Option<ContainerUser>,
    pub command: Option<String>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub image_name: ImageIdentifier,
    pub mounts: Vec<MountDefinition>,
    pub added_capabilities: Vec<Capability>,
    pub labels: Vec<Label>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_should_parse_namespaced_image() {
        let parsed: VersionedImageName = "openbao/openbao:2.4.3".parse().expect("should parse");

        assert_eq!(
            parsed,
            VersionedImageName::namespaced_specific("openbao", "openbao", "2.4.3")
        );
    }

    #[test]
    fn test_from_str_should_parse_unnamespaced_image() {
        let parsed: VersionedImageName = "nginx:latest".parse().expect("should parse");

        assert_eq!(parsed, VersionedImageName::latest("nginx"));
    }

    #[test]
    fn test_from_str_should_reject_missing_version() {
        let result = "nginx".parse::<VersionedImageName>();

        assert!(matches!(
            result,
            Err(VersionedImageNameParseError::Invalid(_))
        ));
    }

    #[test]
    fn test_from_str_should_reject_empty_name() {
        let result = "namespace/:latest".parse::<VersionedImageName>();

        assert!(matches!(
            result,
            Err(VersionedImageNameParseError::EmptyName)
        ));
    }

    #[test]
    fn test_serialize_then_deserialize_should_round_trip() {
        let original = VersionedImageName::namespaced_specific("openbao", "openbao", "2.4.3");

        let serialized = serde_json::to_string(&original).expect("should serialize");
        let deserialized: VersionedImageName =
            serde_json::from_str(&serialized).expect("should deserialize");

        assert_eq!(original, deserialized);
    }

    mod id_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_id")]
            id: Id,
        }

        #[test]
        fn should_deserialize_ids() {
            let json = r#"
                {
                  "id": "alg:123456"
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                Id {
                    algorithm: "alg".to_string(),
                    hex: "123456".to_string()
                },
                wrapper.id
            );
        }

        #[test]
        fn should_deserialize_ids_without_alg() {
            let json = r#"
                {
                  "id": "123456"
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(
                Id {
                    algorithm: "missing-algorithim".to_string(),
                    hex: "123456".to_string()
                },
                wrapper.id
            );
        }
    }

    mod labels_deserializer {
        use super::super::*;
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_err_if_invalid_format() {
            let json = r#"
                {
                    "labels": 4
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_err_if_invalid_data() {
            let json = r#"
                {
                    "labels": {"magic": 42}
                }
            "#;

            let result = serde_json::from_str::<Wrapper>(json);
            assert!(result.is_err());
        }

        #[test]
        fn should_deserialize_labels() {
            let json = r#"
                {
                    "labels": {
                        "foo": "bar",
                        "baz": "qux"
                    }
                }
            "#;

            let wrapper: Wrapper = serde_json::from_str(json).unwrap();

            assert_eq!(
                vec![Label::new("baz", "qux"), Label::new("foo", "bar")],
                wrapper.labels
            );
        }
    }

    mod labels_serializer {
        use super::super::*;

        use serde_json::Value;
        use serde_json::json;

        #[derive(Debug, Serialize)]
        struct Wrapper {
            #[serde(serialize_with = "serialize_labels")]
            labels: Vec<Label>,
        }

        #[test]
        fn should_serialize_labels() {
            let wrapper = Wrapper {
                labels: vec![Label::new("foo", "bar"), Label::new("baz", "qux")],
            };

            let json_str = serde_json::to_string(&wrapper).unwrap();
            let expected = json!({
                "labels": {
                    "foo": "bar",
                    "baz": "qux"
                }
            });

            let actual: Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(expected, actual);
        }
    }
}
