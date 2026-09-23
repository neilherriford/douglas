use crate::{ImagePathComponent, Version, VersionedImageName};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePath(Vec<ImagePathComponent>);

impl ImagePath {
    pub fn new(components: Vec<ImagePathComponent>) -> Option<Self> {
        if components.is_empty() {
            None
        } else {
            Some(Self(components))
        }
    }
}

impl fmt::Display for ImagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined: Vec<&str> = self.0.iter().map(|component| component.as_ref()).collect();
        formatter.write_str(&joined.join("/"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullTarget {
    pub path: ImagePath,
    pub version: Version,
}

impl PullTarget {
    pub fn latest(name: &str) -> Self {
        VersionedImageName::latest(name).into()
    }

    pub fn specific(name: &str, version: &str) -> Self {
        VersionedImageName::specific(name, version).into()
    }

    pub fn namespaced_latest(namespace: &str, name: &str) -> Self {
        VersionedImageName::namespaced_latest(namespace, name).into()
    }

    pub fn namespaced_specific(namespace: &str, name: &str, version: &str) -> Self {
        VersionedImageName::namespaced_specific(namespace, name, version).into()
    }

    pub fn formatted_name(&self) -> String {
        self.path.to_string()
    }

    pub fn version_formatted_name(&self) -> String {
        format!("{}:{}", self.path, self.version)
    }
}

impl From<VersionedImageName> for PullTarget {
    fn from(name: VersionedImageName) -> Self {
        let mut components = Vec::with_capacity(2);
        if let Some(namespace) = name.namespace {
            components.push(namespace);
        }
        components.push(name.name);

        PullTarget {
            path: ImagePath::new(components).expect("at least the image name is always present"),
            version: name.version,
        }
    }
}

impl fmt::Display for PullTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.version_formatted_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    mod image_path {
        use super::*;

        #[test]
        fn test_new_should_reject_an_empty_path() {
            assert!(ImagePath::new(Vec::new()).is_none());
        }

        #[test]
        fn test_display_should_join_components_with_slashes() {
            let path = ImagePath::new(vec![
                ImagePathComponent::from_str("docker.io").unwrap(),
                ImagePathComponent::from_str("library").unwrap(),
                ImagePathComponent::from_str("nginx").unwrap(),
            ])
            .unwrap();

            assert_eq!(path.to_string(), "docker.io/library/nginx");
        }
    }

    mod pull_target {
        use super::*;

        #[test]
        fn test_formatted_name_should_join_the_path() {
            let path = ImagePath::new(vec![
                ImagePathComponent::from_str("docker.io").unwrap(),
                ImagePathComponent::from_str("library").unwrap(),
                ImagePathComponent::from_str("nginx").unwrap(),
            ])
            .unwrap();
            let target = PullTarget {
                path,
                version: Version::from_str("1.27").unwrap(),
            };

            assert_eq!(target.formatted_name(), "docker.io/library/nginx");
            assert_eq!(
                target.version_formatted_name(),
                "docker.io/library/nginx:1.27"
            );
            assert_eq!(target.to_string(), "docker.io/library/nginx:1.27");
        }

        #[test]
        fn test_from_versioned_image_name_with_namespace() {
            let name = VersionedImageName::namespaced_specific("openbao", "openbao", "2.4.3");

            let target = PullTarget::from(name);

            assert_eq!(target.formatted_name(), "openbao/openbao");
            assert_eq!(target.version_formatted_name(), "openbao/openbao:2.4.3");
        }

        #[test]
        fn test_from_versioned_image_name_without_namespace() {
            let name = VersionedImageName::specific("nginx", "1.27");

            let target = PullTarget::from(name);

            assert_eq!(target.formatted_name(), "nginx");
            assert_eq!(target.version_formatted_name(), "nginx:1.27");
        }

        #[test]
        fn test_latest_should_match_versioned_image_name_latest() {
            let target = PullTarget::latest("nginx");

            assert_eq!(target, VersionedImageName::latest("nginx").into());
        }

        #[test]
        fn test_specific_should_match_versioned_image_name_specific() {
            let target = PullTarget::specific("nginx", "1.27");

            assert_eq!(target, VersionedImageName::specific("nginx", "1.27").into());
        }

        #[test]
        fn test_namespaced_latest_should_match_versioned_image_name_namespaced_latest() {
            let target = PullTarget::namespaced_latest("openbao", "openbao");

            assert_eq!(
                target,
                VersionedImageName::namespaced_latest("openbao", "openbao").into()
            );
        }

        #[test]
        fn test_namespaced_specific_should_match_versioned_image_name_namespaced_specific() {
            let target = PullTarget::namespaced_specific("openbao", "openbao", "2.4.3");

            assert_eq!(
                target,
                VersionedImageName::namespaced_specific("openbao", "openbao", "2.4.3").into()
            );
        }
    }
}
