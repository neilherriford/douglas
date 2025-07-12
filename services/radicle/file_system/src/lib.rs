use async_trait::async_trait;
use mockall::automock;
use std::ffi::OsString;
use std::fs::{
    File, Metadata, Permissions as Perms, create_dir_all, metadata, read_dir, read_link,
    read_to_string, remove_file, rename, set_permissions,
};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, chown, symlink};

use std::path::{Path, PathBuf};
use thiserror::Error;

use users::{get_group_by_name, get_user_by_name};

#[derive(Error, Debug)]
pub enum FileSystemError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Not found {0}")]
    NotFoundError(PathBuf),
    #[error("Group not found {0}")]
    GroupNotFoundError(String),
    #[error("User not found {0}")]
    UserNotFoundError(String),
    #[error("Invalid file name")]
    InvalidFileNameError(OsString),
    #[error("Expected path to be a file")]
    ExpectedFileError,
}

#[repr(u32)]
#[derive(PartialEq, Eq, Debug)]
pub enum Modes {
    None = 0,
    OwnerReadWrite = 0o600,
    OwnerAndGroupReadWrite = 0o660,
    Other(u32),
}

impl From<Modes> for u32 {
    fn from(value: Modes) -> Self {
        match value {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerAndGroupReadWrite => 0o660,
            Modes::Other(value) => value,
        }
    }
}

impl From<&Modes> for u32 {
    fn from(value: &Modes) -> Self {
        match value {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerAndGroupReadWrite => 0o660,
            Modes::Other(value) => *value,
        }
    }
}

impl std::fmt::Display for Modes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerAndGroupReadWrite => 0o660,
            Modes::Other(value) => *value,
        };

        write!(f, "0o{:o}", value)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub is_link: bool,
}

impl Entry {
    fn try_create_entry(
        name: OsString,
        metadata: Result<Metadata, std::io::Error>,
    ) -> Result<Entry, FileSystemError> {
        let entry_name = match name.to_str() {
            Some(name) => name.to_string(),
            None => {
                return Err(FileSystemError::InvalidFileNameError(name));
            }
        };

        let kind;
        let is_link;

        match metadata {
            Ok(metadata) => {
                if metadata.is_file() {
                    kind = EntryKind::File;
                } else {
                    kind = EntryKind::Directory;
                }
                is_link = metadata.is_symlink();
            }
            Err(err) => return Err(FileSystemError::IoError(err)),
        }

        Ok(Entry {
            name: entry_name,
            kind,
            is_link,
        })
    }
}

#[automock]
pub trait FileWriter {
    fn write_all(&self, path: &Path, contents: String) -> Result<(), FileSystemError>;
}

#[automock]
pub trait FileReader {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError>;
}

#[automock]
pub trait FileDeleter {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError>;
}

#[automock]
pub trait FileRenamer {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
}

pub struct LocalFileWriter {}

impl LocalFileWriter {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileWriter for LocalFileWriter {
    fn write_all(&self, path: &Path, contents: String) -> Result<(), FileSystemError> {
        let mut file = File::create(&path)?;
        file.write_all(contents.as_bytes())?;

        Ok(())
    }
}

pub struct LocalFileReader {}

impl LocalFileReader {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileReader for LocalFileReader {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError> {
        Ok(read_to_string(path)?)
    }
}

pub struct LocalFileDeleter {}

impl LocalFileDeleter {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileDeleter for LocalFileDeleter {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError> {
        if path.exists() {
            remove_file(path)?;
        }
        Ok(())
    }
}

pub struct LocalFileRenamer {}

impl LocalFileRenamer {
    pub fn new() -> Self {
        Self {}
    }
}
impl FileRenamer for LocalFileRenamer {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
        Ok(rename(from, to)?)
    }
}

#[automock]
pub trait Permissions {
    fn change_user_and_group_ownership(
        &self,
        path: &Path,
        user_name: &str,
        group_name: &str,
    ) -> Result<(), FileSystemError>;
    fn change_mode(&self, path: &Path, mode: &Modes) -> Result<(), FileSystemError>;
}

pub struct LocalPermissions {}

impl LocalPermissions {
    pub fn new() -> Self {
        Self {}
    }
}

impl Permissions for LocalPermissions {
    fn change_user_and_group_ownership(
        &self,
        path: &Path,
        user_name: &str,
        group_name: &str,
    ) -> Result<(), FileSystemError> {
        if let Some(group) = get_group_by_name(group_name) {
            if let Some(user) = get_user_by_name(user_name) {
                Ok(chown(path, Some(user.uid()), Some(group.gid()))?)
            } else {
                Err(FileSystemError::UserNotFoundError(user_name.to_string()))
            }
        } else {
            Err(FileSystemError::GroupNotFoundError(group_name.to_string()))
        }
    }

    fn change_mode(&self, path: &Path, mode: &Modes) -> Result<(), FileSystemError> {
        Ok(set_permissions(path, Perms::from_mode(mode.into()))?)
    }
}

#[async_trait]
#[automock]
pub trait Listener {
    async fn accept(
        &self,
    ) -> Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr), std::io::Error>;
}

#[async_trait]
impl Listener for tokio::net::UnixListener {
    async fn accept(
        &self,
    ) -> Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr), std::io::Error> {
        tokio::net::UnixListener::accept(self).await
    }
}

#[automock]
pub trait UnixDomainSocket {
    fn bind(
        &self,
        path: &Path,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, FileSystemError>;
}

pub struct LocalUnixDomainSocket {}

impl LocalUnixDomainSocket {
    pub fn new() -> Self {
        Self {}
    }
}

impl UnixDomainSocket for LocalUnixDomainSocket {
    fn bind(
        &self,
        path: &Path,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, FileSystemError> {
        Ok(Box::new(tokio::net::UnixListener::bind(path)?))
    }
}

#[automock]
pub trait Links {
    fn create(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
    fn read(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
}

pub struct LocalLinks {}

impl LocalLinks {
    pub fn new() -> Self {
        Self {}
    }
}

impl Links for LocalLinks {
    fn create(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
        Ok(symlink(from, to)?)
    }

    fn read(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        Ok(read_link(path)?)
    }
}

#[automock]
pub trait Inspect {
    fn is_directory(&self, path: &Path) -> bool;
    fn read_metadata(&self, path: &Path) -> Result<Entry, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
}

pub struct LocalInspect {}

impl LocalInspect {
    pub fn new() -> Self {
        Self {}
    }
}

impl Inspect for LocalInspect {
    fn is_directory(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read_metadata(&self, path: &Path) -> Result<Entry, FileSystemError> {
        let name = match path.file_name() {
            Some(name) => name.into(),
            None => {
                return Err(FileSystemError::ExpectedFileError);
            }
        };
        Entry::try_create_entry(name, metadata(path))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[automock]
pub trait Directory {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn create_dir_all(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn entries(&self, path: &Path) -> Result<Vec<Entry>, FileSystemError>;
}

pub struct LocalDirectory {}

impl LocalDirectory {
    pub fn new() -> Self {
        Self {}
    }
}

impl Directory for LocalDirectory {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        Ok(path.canonicalize()?)
    }

    fn create_dir_all(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        create_dir_all(path)?;
        self.canonicalize(path)
    }

    fn entries(&self, path: &Path) -> Result<Vec<Entry>, FileSystemError> {
        let mut result = Vec::<Entry>::new();

        result.extend(read_dir(path)?.into_iter().filter_map(|entry| {
            let entry = entry.ok()?;
            Some(Entry::try_create_entry(entry.file_name(), entry.metadata()).ok()?)
        }));

        Ok(result)
    }
}
