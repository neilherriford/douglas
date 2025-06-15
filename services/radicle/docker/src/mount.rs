use crate::DockerError;
use crate::SimpleDockerClient;
use crate::file_system::EntryKind;
use once_cell::sync::Lazy;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use regex::Regex;
use std::fmt;
use std::num::ParseIntError;
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u8);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl Add<u8> for Version {
    type Output = Version;
    fn add(self, rhs: u8) -> <Self as Add<u8>>::Output {
        Version(self.0 + rhs)
    }
}

#[derive(Error, Debug)]
pub enum VersionParseError {
    #[error("Invalid format")]
    InvalidFormat,
    #[error("Invalid number")]
    InvalidNumber(#[from] ParseIntError),
}

static VERSION_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^v(?P<version>\d+)$").unwrap());

impl FromStr for Version {
    type Err = VersionParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let capture = VERSION_PATTERN
            .captures(value)
            .ok_or(VersionParseError::InvalidFormat)?;

        let version = capture["version"].parse::<u8>()?;

        Ok(Version(version))
    }
}

// Like NON_ALPHANUMERIC, except allow minus, period, and underscore
const FS_NON_ALPHANUMERIC: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~');

#[derive(Debug, PartialEq)]
pub struct VersionedMount {
    pub path: PathBuf,
    pub version: Version,
}

pub trait Repository {
    fn archive(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<VersionedMount, DockerError>;

    fn availble_versions(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<Vec<Version>, DockerError>;

    fn create(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<VersionedMount, DockerError>;

    fn current_version(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<Option<VersionedMount>, DockerError>;

    fn set_version(
        &self,
        container_name: &String,
        mount_name: &String,
        version: &Version,
    ) -> Result<VersionedMount, DockerError>;
}

impl Repository for SimpleDockerClient {
    fn archive(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<VersionedMount, DockerError> {
        let mut mount_base = self.container_mount_base(&container_name, &mount_name);
        let mut versions = self.availble_versions(&container_name, &mount_name)?;
        let next_version = match versions.pop() {
            Some(version) => version + 1,
            None => {
                return Err(DockerError::PathError {
                    path: mount_base,
                    message: "No versions present, cannot archive".to_string(),
                });
            }
        };

        let result = self.container_mount_current(&container_name, &mount_name);

        mount_base = self.create_version_path(mount_base, next_version);
        let new_version_path = mount_base.as_path();
        self.fs.create_dir_all(&new_version_path)?;
        self.force_link(&result, new_version_path)?;

        Ok(VersionedMount {
            path: result,
            version: next_version,
        })
    }

    fn availble_versions(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<Vec<Version>, DockerError> {
        let mount_base = self.container_mount_base(&container_name, &mount_name);

        let mut result = Vec::<Version>::new();

        if !self.fs.exists(&mount_base) {
            return Err(DockerError::PathError {
                path: mount_base,
                message: "Missing mount".to_string(),
            });
        }

        result.extend(
            self.fs
                .entries(mount_base.as_path())?
                .into_iter()
                .filter_map(|entry| {
                    if entry.kind == EntryKind::File {
                        return None;
                    }

                    let version = entry.name.parse::<Version>().ok()?;
                    Some(version)
                }),
        );

        result.sort();
        Ok(result)
    }

    fn create(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<VersionedMount, DockerError> {
        let mut mount_base = self.container_mount_base(&container_name, &mount_name);

        if !self.fs.exists(&mount_base) {
            self.fs.create_dir_all(&mount_base)?;
        }

        let result = self.container_mount_current(&container_name, &mount_name);
        let next_version = match self.availble_versions(container_name, mount_name) {
            Ok(mut versions) => {
                if let Some(last_version) = versions.pop() {
                    last_version + 1
                } else {
                    Version(0)
                }
            }
            Err(err) => return Err(err),
        };

        mount_base = self.create_version_path(mount_base, next_version);
        self.fs.create_dir_all(&mount_base)?;

        self.force_link(&result, &mount_base)?;
        return Ok(VersionedMount {
            path: result,
            version: next_version,
        });
    }

    fn current_version(
        &self,
        container_name: &String,
        mount_name: &String,
    ) -> Result<Option<VersionedMount>, DockerError> {
        let current = self.container_mount_current(&container_name, &mount_name);

        if !self.fs.exists(current.as_path()) {
            return Ok(None);
        }

        let entry = self.fs.read_metadata(&current)?;
        if !entry.is_link {
            return Err(DockerError::PathError {
                path: current,
                message: "Expected to be symlink".to_string(),
            });
        }

        let original = self.fs.read_link(&current)?;

        if let Some(directory) = original.file_name() {
            let directory = match directory.to_str() {
                Some(directory) => directory,
                _ => {
                    return Err(DockerError::PathError {
                        path: original,
                        message: "Non UTF8 path".to_string(),
                    });
                }
            };

            match directory.parse::<Version>() {
                Ok(version) => Ok(Some(VersionedMount {
                    path: current,
                    version,
                })),
                _ => Ok(None),
            }
        } else {
            Err(DockerError::PathError {
                path: original,
                message: "No top level directory".to_string(),
            })
        }
    }

    fn set_version(
        &self,
        container_name: &String,
        mount_name: &String,
        version: &Version,
    ) -> Result<VersionedMount, DockerError> {
        let avaliable = self.availble_versions(&container_name, &mount_name)?;

        if avaliable.len() == 0 {
            return Err(DockerError::InvalidArgumentError {
                name: "version".to_string(),
                given: format!("{}", version),
                message: "No availble versions".to_string(),
            });
        }

        if !avaliable.contains(&version) {
            let avaliable_list = avaliable
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            return Err(DockerError::InvalidArgumentError {
                name: "version".to_string(),
                given: format!("{}", version),
                message: format!("Expected version to be one of {}", avaliable_list),
            });
        }

        let container_mount_base = self.container_mount_base(&container_name, &mount_name);
        let new_version = self.create_version_path(container_mount_base, *version);
        let current = self.container_mount_current(&container_name, &mount_name);
        self.force_link(&current, &new_version)?;

        Ok(VersionedMount {
            path: current,
            version: *version,
        })
    }
}

impl SimpleDockerClient {
    fn container_mount_current(&self, container_name: &String, mount_name: &String) -> PathBuf {
        let mut result = self.container_mount_base(container_name, mount_name);
        result.push("current");
        result
    }

    fn create_version_path(&self, mut container_mount_base: PathBuf, version: Version) -> PathBuf {
        container_mount_base.push(version.to_string());
        container_mount_base
    }

    fn container_mount_base(&self, container_name: &String, mount_name: &String) -> PathBuf {
        let safe_container_name = to_safe_name(container_name.to_string());
        let safe_mount_name = to_safe_name(mount_name.to_string());
        let mut result = self.mount_root.clone();

        result.push(safe_container_name);
        result.push(safe_mount_name);

        return result;
    }

    fn force_link(&self, linked: &PathBuf, source: &Path) -> Result<(), DockerError> {
        if self.fs.exists(&linked) {
            self.fs.delete(&linked)?;
        }
        self.fs.link(&source, &linked)?;
        Ok(())
    }
}

fn to_safe_name(input: String) -> String {
    utf8_percent_encode(&input, FS_NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    mod repository {
        mod archive {
            use super::super::super::*;
            use crate::Path;
            use crate::file_system::{Entry, MockFileSystem};
            use simple_rest_client::MockRestClient;

            #[test]
            fn should_err_if_invalid_directory() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_does_not_exist("/tmp/foo/bar%20mount");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());

                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message }) if path == Path::new("/tmp/foo/bar%20mount").to_path_buf() && message.contains("Missing")
                ));
            }

            #[test]
            fn should_err_if_no_versions() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries("/tmp/foo/bar%20mount", vec![]);

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message }) if path == Path::new("/tmp/foo/bar%20mount").to_path_buf() && message.contains("No versions")
                ));
            }

            #[test]
            fn should_err_if_only_files() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "baz".to_string(),
                        kind: EntryKind::File,
                        is_link: false,
                    }],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message }) if path == Path::new("/tmp/foo/bar%20mount").to_path_buf() && message.contains("No versions")
                ));
            }

            #[test]
            fn should_err_if_directory_name_doesnt_match() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "baz".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message }) if path == Path::new("/tmp/foo/bar%20mount").to_path_buf() && message.contains("No versions")
                ));
            }

            #[test]
            fn should_create_and_link_new_version() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "v0".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );
                mock_fs.expect_create_dir_all_for_path("/tmp/foo/bar%20mount/v1");
                mock_fs.expect_path_does_not_exist("/tmp/foo/bar%20mount/current");
                mock_fs.expect_link_to_paths(
                    "/tmp/foo/bar%20mount/v1",
                    "/tmp/foo/bar%20mount/current",
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());

                assert!(matches!(
                    result,
                    Ok(VersionedMount { path, version}) if path == Path::new("/tmp/foo/bar%20mount/current") && version == Version(1)
                ));
            }

            #[test]
            fn should_create_and_force_link_new_version() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "v0".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );
                mock_fs.expect_create_dir_all_for_path("/tmp/foo/bar%20mount/v1");
                mock_fs.expect_path_exists("/tmp/foo/bar%20mount/current");
                mock_fs.expect_delete_path("/tmp/foo/bar%20mount/current");
                mock_fs.expect_link_to_paths(
                    "/tmp/foo/bar%20mount/v1",
                    "/tmp/foo/bar%20mount/current",
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.archive(&"foo".to_string(), &"bar mount".to_string());

                assert!(matches!(
                    result,
                    Ok(VersionedMount { path, version}) if path == Path::new("/tmp/foo/bar%20mount/current") && version == Version(1)
                ));
            }
        }

        mod availble_versions {
            use super::super::super::*;
            use crate::Path;
            use crate::file_system::{Entry, MockFileSystem};
            use simple_rest_client::MockRestClient;

            #[test]
            fn should_err_if_invalid_directory() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_does_not_exist("/tmp/foo/bar%20mount");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message }) if path == Path::new("/tmp/foo/bar%20mount").to_path_buf() && message.contains("Missing")
                ));
            }

            #[test]
            fn should_return_empty_if_no_directories() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries("/tmp/foo/bar%20mount", vec![]);

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(result, Ok(entries) if entries == Vec::<Version>::new()));
            }

            #[test]
            fn should_err_if_only_files() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "baz".to_string(),
                        kind: EntryKind::File,
                        is_link: false,
                    }],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(result, Ok(entries) if entries == Vec::<Version>::new()));
            }

            #[test]
            fn should_return_empty_if_bad_directory_names() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "baz".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(result, Ok(entries) if entries == Vec::<Version>::new()));
            }

            #[test]
            fn should_return_versions() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![Entry {
                        name: "v0".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());
                assert!(matches!(result, Ok(entries) if entries == vec![Version(0)]));
            }

            #[test]
            fn should_return_versions_sorted() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar%20mount",
                    vec![
                        Entry {
                            name: "v1".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                        Entry {
                            name: "v0".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                    ],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.availble_versions(&"foo".to_string(), &"bar mount".to_string());

                assert!(matches!(result, Ok(entries) if entries == vec![Version(0), Version(1)]));
            }
        }

        mod create {
            use super::super::super::*;
            use crate::Path;
            use crate::file_system::{Entry, MockFileSystem};
            use std::collections::HashSet;
            use std::sync::Arc;
            use std::sync::Mutex;

            use simple_rest_client::MockRestClient;

            #[test]
            fn should_create_directories_if_net_new() {
                let mut mock_fs = MockFileSystem::new();
                let tracking: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

                mock_fs.expect_exists_with_tracking(&tracking);
                mock_fs.expect_create_dir_all_with_tracking(&tracking);
                mock_fs.expect_entries_for_path("/tmp/foo/bar", vec![]);
                mock_fs.expect_link_to_paths("/tmp/foo/bar/v0", "/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.create(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(
                    result,
                    Ok(versioned_path) if versioned_path == VersionedMount { path: Path::new("/tmp/foo/bar/current").to_path_buf(), version: Version(0)}
                ));
            }

            #[test]
            fn should_not_recreate_base() {
                let mut mock_fs = MockFileSystem::new();

                let tracking: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar");

                mock_fs.expect_exists_with_tracking(&tracking);
                mock_fs.expect_create_dir_all_with_tracking(&tracking);
                mock_fs.expect_entries_for_path("/tmp/foo/bar", vec![]);
                mock_fs.expect_link_to_paths("/tmp/foo/bar/v0", "/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.create(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(
                    result,
                    Ok(versioned_path) if versioned_path == VersionedMount { path: Path::new("/tmp/foo/bar/current").to_path_buf(), version: Version(0)}
                ));
                assert!(MockFileSystem::tracking_contains(
                    &tracking,
                    "/tmp/foo/bar/v0"
                ))
            }

            #[test]
            fn should_hard_relink() {
                let mut mock_fs = MockFileSystem::new();

                let tracking: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar");
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar/current");

                mock_fs.expect_exists_with_tracking(&tracking);
                mock_fs.expect_create_dir_all_with_tracking(&tracking);

                mock_fs.expect_entries_for_path(
                    "/tmp/foo/bar",
                    vec![Entry {
                        name: "current".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    }],
                );
                mock_fs.expect_delete_path("/tmp/foo/bar/current");
                mock_fs.expect_link_to_paths("/tmp/foo/bar/v0", "/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.create(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(
                    result,
                    Ok(versioned_path) if versioned_path == VersionedMount { path: Path::new("/tmp/foo/bar/current").to_path_buf(), version: Version(0)}
                ));
                assert!(MockFileSystem::tracking_contains(
                    &tracking,
                    "/tmp/foo/bar/v0"
                ))
            }

            #[test]
            fn should_create_new_veresion() {
                let mut mock_fs = MockFileSystem::new();

                let tracking: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar");
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar/current");
                MockFileSystem::add_path_to_tracking(&tracking, "/tmp/foo/bar/v0");

                mock_fs.expect_exists_with_tracking(&tracking);
                mock_fs.expect_create_dir_all_with_tracking(&tracking);

                mock_fs.expect_entries_for_path(
                    "/tmp/foo/bar",
                    vec![
                        Entry {
                            name: "current".to_string(),
                            kind: EntryKind::Directory,
                            is_link: true,
                        },
                        Entry {
                            name: "v0".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                    ],
                );
                mock_fs.expect_delete_path("/tmp/foo/bar/current");
                mock_fs.expect_link_to_paths("/tmp/foo/bar/v1", "/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.create(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(
                    result,
                    Ok(versioned_path) if versioned_path == VersionedMount { path: Path::new("/tmp/foo/bar/current").to_path_buf(), version: Version(1)}
                ));
                assert!(MockFileSystem::tracking_contains(
                    &tracking,
                    "/tmp/foo/bar/v1"
                ))
            }
        }

        mod current_version {
            use super::super::super::*;
            use crate::Path;
            use crate::file_system::{Entry, MockFileSystem};
            use simple_rest_client::MockRestClient;

            #[test]
            fn should_return_none_if_no_current() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_does_not_exist("/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.current_version(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(result, Ok(None)));
            }

            #[test]
            fn should_return_error_if_current_is_not_a_symlink() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists("/tmp/foo/bar/current");
                mock_fs.expect_read_metadata_with_path(
                    "/tmp/foo/bar/current",
                    Entry {
                        name: "current".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    },
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.current_version(&"foo".to_string(), &"bar".to_string());

                assert!(
                    matches!(result, Err(DockerError::PathError { path, message })
                    if path == Path::new("/tmp/foo/bar/current")
                    && message.contains(&"symlink".to_string()))
                );
            }

            #[test]
            fn should_return_none_if_current_is_not_a_versioned_mount() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists("/tmp/foo/bar/current");
                mock_fs.expect_read_metadata_with_path(
                    "/tmp/foo/bar/current",
                    Entry {
                        name: "current".to_string(),
                        kind: EntryKind::Directory,
                        is_link: true,
                    },
                );
                mock_fs.expect_read_link_with_path("/tmp/foo/bar/current", "/tmp/foo/bar/oops");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.current_version(&"foo".to_string(), &"bar".to_string());

                assert!(matches!(result, Ok(None)));
            }

            #[test]
            fn should_return_version() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists("/tmp/foo/bar/current");
                mock_fs.expect_read_metadata_with_path(
                    "/tmp/foo/bar/current",
                    Entry {
                        name: "current".to_string(),
                        kind: EntryKind::Directory,
                        is_link: true,
                    },
                );
                mock_fs.expect_read_link_with_path("/tmp/foo/bar/current", "/tmp/foo/bar/v0");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };

                let result = client.current_version(&"foo".to_string(), &"bar".to_string());

                assert!(
                    matches!(result, Ok(Some(versioned_mount)) if versioned_mount.version == Version(0))
                );
            }
        }

        mod set_version {
            use super::super::super::*;
            use crate::Path;
            use crate::file_system::{Entry, MockFileSystem};
            use simple_rest_client::MockRestClient;

            #[test]
            fn should_error_if_no_mount() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_does_not_exist("/tmp/foo/bar");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };
                let result =
                    client.set_version(&"foo".to_string(), &"bar".to_string(), &Version(0));

                assert!(matches!(
                    result,
                    Err(DockerError::PathError { path, message })
                    if path == Path::new("/tmp/foo/bar") && message == "Missing mount"
                ));
            }

            #[test]
            fn should_error_if_no_versions() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries("/tmp/foo/bar", vec![]);

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };
                let result =
                    client.set_version(&"foo".to_string(), &"bar".to_string(), &Version(0));

                assert!(matches!(
                    result,
                    Err(DockerError::InvalidArgumentError { name, given , message })
                    if name == "version"  && given  == "v0" && message == "No availble versions"
                ));
            }

            #[test]
            fn should_error_if_requested_version_not_present() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar",
                    vec![
                        Entry {
                            name: "v0".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                        Entry {
                            name: "v1".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                    ],
                );

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };
                let result =
                    client.set_version(&"foo".to_string(), &"bar".to_string(), &Version(2));

                assert!(matches!(
                    result,
                    Err(DockerError::InvalidArgumentError { name, given , message })
                    if name == "version"  && given  == "v2" && message == "Expected version to be one of v0, v1"
                ));
            }

            #[test]
            fn should_set_version() {
                let mut mock_fs = MockFileSystem::new();
                mock_fs.expect_path_exists_with_entries(
                    "/tmp/foo/bar",
                    vec![
                        Entry {
                            name: "v0".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                        Entry {
                            name: "v1".to_string(),
                            kind: EntryKind::Directory,
                            is_link: false,
                        },
                    ],
                );
                mock_fs.expect_path_exists("/tmp/foo/bar/current");
                mock_fs.expect_delete_path("/tmp/foo/bar/current");
                mock_fs.expect_link_to_paths("/tmp/foo/bar/v1", "/tmp/foo/bar/current");

                let client = SimpleDockerClient {
                    rest_client: Box::new(MockRestClient::new()),
                    mount_root: Path::new("/tmp/").to_path_buf(),
                    fs: Box::new(mock_fs),
                };
                let result =
                    client.set_version(&"foo".to_string(), &"bar".to_string(), &Version(1));

                assert!(matches!(
                    result,
                    Ok(VersionedMount {path, version})
                    if path == Path::new("/tmp/foo/bar/current") && version == Version(1)
                ));
            }
        }
    }

    mod version {
        mod from_str {
            use super::super::super::*;

            #[test]
            fn should_err_if_invalid_format() {
                let result = "oops".parse::<Version>();

                assert!(matches!(result, Err(VersionParseError::InvalidFormat)));
            }

            #[test]
            fn should_err_if_excessive_version() {
                let result = "v999".parse::<Version>();

                assert!(matches!(result, Err(VersionParseError::InvalidNumber(_))));
            }

            #[test]
            fn should_parse() {
                let result = "v123".parse::<Version>();

                assert!(matches!(result, Ok(version) if version == Version(123)));
            }
        }
    }

    mod fn_to_safe_name {
        use super::super::*;

        #[test]
        fn should_not_escape_minus() {
            let actual = to_safe_name("-".to_string());
            assert_eq!(actual, "-".to_string())
        }
        #[test]
        fn should_not_escape_period() {
            let actual = to_safe_name(".".to_string());
            assert_eq!(actual, ".".to_string())
        }

        #[test]
        fn should_not_escape_underscore() {
            let actual = to_safe_name("_".to_string());
            assert_eq!(actual, "_".to_string())
        }
    }
}
