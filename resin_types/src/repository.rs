use crate::{Name, NameParseError, RepositoryComponent};
use docker_types::Registry;
use std::path::PathBuf;
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("invalid repository component: {0}")]
    InvalidComponent(#[from] NameParseError),
    #[error("unrecognized or unsupported upstream registry: {0}")]
    UnknownUpstream(String),
    #[error("{0} is not a local repository")]
    NotLocal(String),
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum Upstream {
    DockerHub,
    Other(Registry),
}

impl Upstream {
    pub fn canonical_host(&self) -> String {
        match self {
            Upstream::DockerHub => "docker.io".to_string(),
            Upstream::Other(registry) => registry.as_ref().to_string(),
        }
    }

    pub fn api_host(&self) -> String {
        match self {
            Upstream::DockerHub => "registry-1.docker.io".to_string(),
            Upstream::Other(registry) => registry.as_ref().to_string(),
        }
    }
}

impl FromStr for Upstream {
    type Err = RepositoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "docker.io" | "index.docker.io" | "registry-1.docker.io" => Ok(Upstream::DockerHub),
            host => host
                .parse::<Registry>()
                .map(Upstream::Other)
                .map_err(|_| RepositoryError::UnknownUpstream(value.to_string())),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct RepositoryPath(Vec<RepositoryComponent>);

impl RepositoryPath {
    fn for_upstream(upstream: &Upstream, segments: &[&str]) -> Result<Self, RepositoryError> {
        let mut components = segments
            .iter()
            .map(|segment| segment.parse::<RepositoryComponent>())
            .collect::<Result<Vec<_>, _>>()?;
        if *upstream == Upstream::DockerHub && components.len() == 1 {
            components.insert(0, "library".parse()?);
        }
        Ok(Self(components))
    }
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined: Vec<&str> = self.0.iter().map(|component| component.as_ref()).collect();
        formatter.write_str(&joined.join("/"))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum Repository {
    Local(Name),
    Upstream {
        upstream: Upstream,
        path: RepositoryPath,
    },
}

fn looks_like_host(segment: &str) -> bool {
    segment.contains('.') || segment.contains(':') || segment == "localhost"
}

impl FromStr for Repository {
    type Err = RepositoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let segments: Vec<&str> = value.split('/').collect();
        match segments.as_slice() {
            [single] => Ok(Repository::Local(single.parse()?)),
            [namespace, name] if !looks_like_host(namespace) => {
                Ok(Repository::Local(Name::from_namespaced(namespace, name)?))
            }
            [host, rest @ ..] if looks_like_host(host) => {
                let upstream: Upstream = host.parse()?;
                let path = RepositoryPath::for_upstream(&upstream, rest)?;
                Ok(Repository::Upstream { upstream, path })
            }
            _ => Err(RepositoryError::UnknownUpstream(value.to_string())),
        }
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Repository::Local(name) => write!(formatter, "{name}"),
            Repository::Upstream { upstream, path } => {
                write!(formatter, "{}/{path}", upstream.canonical_host())
            }
        }
    }
}

impl Repository {
    pub fn storage_path(&self) -> PathBuf {
        match self {
            Repository::Local(name) => PathBuf::from("local").join(name.fs_safe()),
            Repository::Upstream { upstream, path } => PathBuf::from("upstream")
                .join(upstream.canonical_host())
                .join(path.to_string().replace('/', "%2F")),
        }
    }

    pub fn as_upstream(&self) -> Option<(&Upstream, &RepositoryPath)> {
        match self {
            Repository::Upstream { upstream, path } => Some((upstream, path)),
            Repository::Local(_) => None,
        }
    }

    pub fn require_local(&self) -> Result<&Name, RepositoryError> {
        match self {
            Repository::Local(name) => Ok(name),
            Repository::Upstream { .. } => Err(RepositoryError::NotLocal(self.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod upstream_from_str {
        use super::*;

        #[test]
        fn test_should_collapse_docker_hub_aliases_to_one_value() {
            assert_eq!(
                "docker.io".parse::<Upstream>().unwrap(),
                Upstream::DockerHub
            );
            assert_eq!(
                "index.docker.io".parse::<Upstream>().unwrap(),
                Upstream::DockerHub
            );
            assert_eq!(
                "registry-1.docker.io".parse::<Upstream>().unwrap(),
                Upstream::DockerHub
            );
        }

        #[test]
        fn test_should_be_case_insensitive() {
            assert_eq!(
                "DOCKER.IO".parse::<Upstream>().unwrap(),
                Upstream::DockerHub
            );
        }

        #[test]
        fn test_should_parse_another_registry_host() {
            let upstream = "ghcr.io".parse::<Upstream>().unwrap();
            assert_eq!(upstream, Upstream::Other("ghcr.io".parse().unwrap()));
        }

        #[test]
        fn test_should_reject_an_invalid_registry_host() {
            assert!(matches!(
                "not a host!".parse::<Upstream>(),
                Err(RepositoryError::UnknownUpstream(_))
            ));
        }
    }

    mod upstream_hosts {
        use super::*;

        #[test]
        fn test_docker_hub_canonical_host_should_be_docker_io() {
            assert_eq!(Upstream::DockerHub.canonical_host(), "docker.io");
        }

        #[test]
        fn test_docker_hub_api_host_should_be_registry_1_docker_io() {
            assert_eq!(Upstream::DockerHub.api_host(), "registry-1.docker.io");
        }

        #[test]
        fn test_other_upstream_uses_the_same_host_for_canonical_and_api() {
            let upstream = Upstream::Other("ghcr.io".parse().unwrap());
            assert_eq!(upstream.canonical_host(), "ghcr.io");
            assert_eq!(upstream.api_host(), "ghcr.io");
        }
    }

    mod repository_from_str {
        use super::*;

        #[test]
        fn test_should_parse_a_single_segment_as_local() {
            let repository: Repository = "hello-world".parse().unwrap();
            assert_eq!(
                repository,
                Repository::Local("hello-world".parse().unwrap())
            );
        }

        #[test]
        fn test_should_parse_a_namespaced_local_seedling_name() {
            let repository: Repository = "hello-world/agent".parse().unwrap();
            assert_eq!(
                repository,
                Repository::Local(Name::from_namespaced("hello-world", "agent").unwrap())
            );
        }

        #[test]
        fn test_should_parse_a_docker_hub_reference_and_add_the_implicit_library_namespace() {
            let repository: Repository = "docker.io/nginx".parse().unwrap();
            assert_eq!(
                repository,
                Repository::Upstream {
                    upstream: Upstream::DockerHub,
                    path: RepositoryPath::for_upstream(&Upstream::DockerHub, &["nginx"]).unwrap(),
                }
            );
            assert_eq!(repository.to_string(), "docker.io/library/nginx");
        }

        #[test]
        fn test_should_parse_a_docker_hub_reference_that_already_has_a_namespace() {
            let repository: Repository = "docker.io/library/nginx".parse().unwrap();
            assert_eq!(repository.to_string(), "docker.io/library/nginx");
        }

        #[test]
        fn test_should_parse_a_multi_segment_ghcr_reference() {
            let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();
            assert_eq!(repository.to_string(), "ghcr.io/foo/bar");
        }

        #[test]
        fn test_should_recognize_localhost_with_a_port_as_a_host() {
            let repository: Repository = "localhost:7376/nginx".parse().unwrap();
            assert!(repository.as_upstream().is_some());
        }

        #[test]
        fn test_should_reject_an_invalid_component() {
            assert!(matches!(
                "docker.io/Not Valid".parse::<Repository>(),
                Err(RepositoryError::InvalidComponent(_))
            ));
        }
    }

    mod storage_path {
        use super::*;

        #[test]
        fn test_local_repository_should_live_under_local() {
            let repository: Repository = "hello-world".parse().unwrap();
            assert_eq!(
                repository.storage_path(),
                PathBuf::from("local/hello-world")
            );
        }

        #[test]
        fn test_upstream_repository_should_live_under_upstream_host() {
            let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();
            assert_eq!(
                repository.storage_path(),
                PathBuf::from("upstream/ghcr.io/foo%2Fbar")
            );
        }

        #[test]
        fn test_local_and_upstream_names_that_could_collide_stay_separate() {
            // A seedling could in principle be named "ghcr.io" — it must not land in
            // the same directory as the upstream tree for the real ghcr.io host.
            let local: Repository = "ghcr.io".parse().unwrap();
            assert_eq!(local.storage_path(), PathBuf::from("local/ghcr.io"));
        }
    }

    mod require_local {
        use super::*;

        #[test]
        fn test_should_return_the_name_for_a_local_repository() {
            let repository: Repository = "hello-world".parse().unwrap();
            assert_eq!(
                repository.require_local().unwrap(),
                &"hello-world".parse::<Name>().unwrap()
            );
        }

        #[test]
        fn test_should_reject_an_upstream_repository() {
            let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();
            assert!(matches!(
                repository.require_local(),
                Err(RepositoryError::NotLocal(_))
            ));
        }
    }

    mod as_upstream {
        use super::*;

        #[test]
        fn test_should_return_none_for_a_local_repository() {
            let repository: Repository = "hello-world".parse().unwrap();
            assert!(repository.as_upstream().is_none());
        }

        #[test]
        fn test_should_return_the_upstream_and_path_for_an_upstream_repository() {
            let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();
            let (upstream, path) = repository.as_upstream().unwrap();
            assert_eq!(*upstream, Upstream::Other("ghcr.io".parse().unwrap()));
            assert_eq!(path.to_string(), "foo/bar");
        }
    }
}
