use crate::{
    ImagePathComponent, ImagePathComponentError, Registry, RegistryError, VersionTag,
    VersionTagError,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImageReferenceError {
    #[error(
        "expected registry/name:version or registry/namespace/name:version, got '{0}' (no implicit registry)"
    )]
    MissingRegistry(String),
    #[error("invalid registry: {0}")]
    InvalidRegistry(RegistryError),
    #[error("expected registry/name:version or registry/namespace/name:version, got '{0}'")]
    Invalid(String),
    #[error("image reference must include a pinned version, e.g. ':1.27'")]
    MissingVersion,
    #[error("image reference version cannot be 'latest'; pin an explicit version")]
    VersionMustBePinned,
    #[error("invalid namespace: {0}")]
    InvalidNamespace(ImagePathComponentError),
    #[error("invalid image name: {0}")]
    InvalidName(ImagePathComponentError),
    #[error("invalid version: {0}")]
    InvalidVersion(VersionTagError),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ImageReference {
    pub registry: Registry,
    pub namespace: Option<ImagePathComponent>,
    pub name: ImagePathComponent,
    pub version: VersionTag,
}

fn looks_like_host(segment: &str) -> bool {
    segment.contains('.') || segment.contains(':') || segment == "localhost"
}

impl ImageReference {
    pub fn formatted_name(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{}/{namespace}/{}", self.registry, self.name),
            None => format!("{}/{}", self.registry, self.name),
        }
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.formatted_name(), self.version)
    }
}

impl FromStr for ImageReference {
    type Err = ImageReferenceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let segments: Vec<&str> = value.split('/').collect();
        let (registry, rest) = match segments.as_slice() {
            [registry, rest @ ..] if looks_like_host(registry) && !rest.is_empty() => {
                (*registry, rest)
            }
            _ => return Err(ImageReferenceError::MissingRegistry(value.to_string())),
        };

        let (namespace, name_and_version) = match rest {
            [name_and_version] => (None, *name_and_version),
            [namespace, name_and_version] => (Some(*namespace), *name_and_version),
            _ => return Err(ImageReferenceError::Invalid(value.to_string())),
        };

        let (name, raw_version) = name_and_version
            .split_once(':')
            .ok_or(ImageReferenceError::MissingVersion)?;

        if raw_version == "latest" {
            return Err(ImageReferenceError::VersionMustBePinned);
        }

        let registry = registry
            .parse::<Registry>()
            .map_err(ImageReferenceError::InvalidRegistry)?;
        let namespace = namespace
            .map(|namespace| {
                namespace
                    .parse::<ImagePathComponent>()
                    .map_err(ImageReferenceError::InvalidNamespace)
            })
            .transpose()?;
        let name = name
            .parse::<ImagePathComponent>()
            .map_err(ImageReferenceError::InvalidName)?;
        let version = raw_version
            .parse::<VersionTag>()
            .map_err(ImageReferenceError::InvalidVersion)?;

        Ok(ImageReference {
            registry,
            namespace,
            name,
            version,
        })
    }
}

impl Serialize for ImageReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ImageReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod from_str {
        use super::*;

        #[test]
        fn test_should_parse_a_registry_and_name_and_version() {
            let reference: ImageReference = "docker.io/nginx:1.27".parse().unwrap();
            assert_eq!(reference.registry.as_ref(), "docker.io");
            assert_eq!(reference.namespace, None);
            assert_eq!(reference.name.as_ref(), "nginx");
            assert_eq!(reference.version.as_ref(), "1.27");
        }

        #[test]
        fn test_should_parse_a_registry_and_namespace_and_name_and_version() {
            let reference: ImageReference = "ghcr.io/foo/bar:2.1".parse().unwrap();
            assert_eq!(reference.registry.as_ref(), "ghcr.io");
            assert_eq!(reference.namespace.as_ref().map(AsRef::as_ref), Some("foo"));
            assert_eq!(reference.name.as_ref(), "bar");
            assert_eq!(reference.version.as_ref(), "2.1");
        }

        #[test]
        fn test_should_parse_a_registry_with_a_port() {
            let reference: ImageReference = "localhost:5000/nginx:1.27".parse().unwrap();
            assert_eq!(reference.registry.as_ref(), "localhost:5000");
        }

        #[test]
        fn test_should_reject_a_bare_image_with_no_registry() {
            assert!(matches!(
                "nginx:1.27".parse::<ImageReference>(),
                Err(ImageReferenceError::MissingRegistry(_))
            ));
        }

        #[test]
        fn test_should_reject_a_missing_version() {
            assert!(matches!(
                "docker.io/nginx".parse::<ImageReference>(),
                Err(ImageReferenceError::MissingVersion)
            ));
        }

        #[test]
        fn test_should_reject_latest_as_the_version() {
            assert!(matches!(
                "docker.io/nginx:latest".parse::<ImageReference>(),
                Err(ImageReferenceError::VersionMustBePinned)
            ));
        }

        #[test]
        fn test_should_reject_more_than_one_namespace_level() {
            assert!(matches!(
                "ghcr.io/foo/bar/baz:1.0".parse::<ImageReference>(),
                Err(ImageReferenceError::Invalid(_))
            ));
        }

        #[test]
        fn test_should_reject_an_invalid_registry_host() {
            assert!(matches!(
                "not a host!/nginx:1.27".parse::<ImageReference>(),
                Err(ImageReferenceError::MissingRegistry(_))
            ));
        }

        #[test]
        fn test_should_reject_an_invalid_namespace() {
            assert!(matches!(
                "ghcr.io/Not Valid/bar:1.0".parse::<ImageReference>(),
                Err(ImageReferenceError::InvalidNamespace(_))
            ));
        }
    }

    mod display {
        use super::*;

        #[test]
        fn test_should_round_trip_without_a_namespace() {
            let reference: ImageReference = "docker.io/nginx:1.27".parse().unwrap();
            assert_eq!(reference.to_string(), "docker.io/nginx:1.27");
        }

        #[test]
        fn test_should_round_trip_with_a_namespace() {
            let reference: ImageReference = "ghcr.io/foo/bar:2.1".parse().unwrap();
            assert_eq!(reference.to_string(), "ghcr.io/foo/bar:2.1");
        }
    }

    mod serde {
        use super::*;

        #[test]
        fn test_should_serialize_as_a_string() {
            let reference: ImageReference = "docker.io/nginx:1.27".parse().unwrap();
            let json = serde_json::to_string(&reference).unwrap();
            assert_eq!(json, "\"docker.io/nginx:1.27\"");
        }

        #[test]
        fn test_should_deserialize_from_a_string() {
            let reference: ImageReference =
                serde_json::from_str("\"ghcr.io/foo/bar:2.1\"").unwrap();
            assert_eq!(reference, "ghcr.io/foo/bar:2.1".parse().unwrap());
        }

        #[test]
        fn test_should_reject_deserializing_an_unpinned_version() {
            let result: Result<ImageReference, _> =
                serde_json::from_str("\"docker.io/nginx:latest\"");
            assert!(result.is_err());
        }
    }
}
