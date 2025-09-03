use async_trait::async_trait;
#[cfg(feature = "mock")]
use mockall::predicate;
use std::ffi::OsString;
use std::fs::{
    File, Metadata, OpenOptions, Permissions as Perms, create_dir_all, metadata, read_dir,
    read_link, read_to_string, remove_file, rename, set_permissions,
};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, chown, symlink};
use std::path::{Path, PathBuf};
use thiserror::Error;
use users::{get_group_by_name, get_user_by_name};

#[derive(Error, Debug)]
pub enum FileSystemError {
    #[error("Parent not found for path: {0}")]
    ParentNotFoundError(PathBuf),
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
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Modes {
    None,
    OwnerReadWrite,
    OwnerReadWriteGroupRead,
    OwnerReadWriteGroupReadWrite,
    Other(u32),
}

impl From<Modes> for u32 {
    fn from(value: Modes) -> Self {
        match value {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerReadWriteGroupRead => 0o640,
            Modes::OwnerReadWriteGroupReadWrite => 0o660,
            Modes::Other(v) => v,
        }
    }
}

impl std::fmt::Display for Modes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode: u32 = self.clone().into();
        write!(f, "0o{:o}", mode)
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

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileWriter {
    fn write_all(&self, path: &Path, contents: String) -> Result<(), FileSystemError>;
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileReader {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError>;
}

#[cfg(feature = "mock")]
impl MockFileReader {
    pub fn given_can_read_all_with_contents(&mut self, path: &str, contents: &str) {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let contents = contents.to_string();

        self.expect_read_all()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(contents.clone()));
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileDeleter {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError>;
}

#[cfg(feature = "mock")]
impl MockFileDeleter {
    pub fn expect_file_to_be_deleted(&mut self, path: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_delete()
            .with(predicate::eq(path.clone()))
            .returning(|_| Ok(()));
        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
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

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileAppender {
    fn append(&self, path: &Path, contents: String) -> Result<(), FileSystemError>;
}

pub struct LocalFileAppender {}

impl LocalFileAppender {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileAppender for LocalFileAppender {
    fn append(&self, path: &Path, contents: String) -> Result<(), FileSystemError> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
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

#[cfg_attr(feature = "mock", mockall::automock)]
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
        Ok(set_permissions(
            path,
            Perms::from_mode(mode.clone().into()),
        )?)
    }
}

#[cfg(feature = "mock")]
impl MockPermissions {
    pub fn expect_ownership_to_be_set(
        &mut self,
        path: &str,
        user_name: &str,
        group_name: &str,
    ) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let user_name = user_name.to_string();
        let group_name = group_name.to_string();

        self.expect_change_user_and_group_ownership()
            .with(
                predicate::eq(path.clone()),
                predicate::eq(user_name),
                predicate::eq(group_name),
            )
            .returning(|_, _, _| Ok(()));

        self
    }
    pub fn expect_ownership_and_mode_to_be_set(
        &mut self,
        path: &str,
        user_name: &str,
        group_name: &str,
        mode: Modes,
    ) -> &mut Self {
        self.expect_ownership_to_be_set(path, user_name, group_name);

        let path = path.to_string();
        let path = PathBuf::from(path);
        self.expect_change_mode()
            .with(predicate::eq(path.clone()), predicate::eq(mode))
            .returning(|_, _| Ok(()));

        self
    }
}

#[async_trait]
#[cfg_attr(feature = "mock", mockall::automock)]
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

#[cfg_attr(feature = "mock", mockall::automock)]
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

#[cfg(feature = "mock")]
impl MockUnixDomainSocket {
    pub fn expect_bind_with<F>(&mut self, path: &str, factory: F) -> &mut Self
    where
        F: Fn() -> Box<dyn Listener + Send + Sync + 'static> + Send + 'static,
    {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_bind()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(factory()));

        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
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

#[cfg(feature = "mock")]
impl MockLinks {
    pub fn given_symlink(&mut self, from: &str, to: &str) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_read()
            .with(predicate::eq(from.clone()))
            .returning(move |_| Ok(to.clone()));

        self
    }

    pub fn expect_create_with(&mut self, from: &str, to: &str) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_create()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .returning(|_, _| Ok(()));

        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
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

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Folder {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn create_recursively(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn entries(&self, path: &Path) -> Result<Vec<Entry>, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
    fn pop(&self, path: &Path) -> Option<String>;
    fn executable_root(&self) -> Result<PathBuf, FileSystemError>;
    fn create_file(&self, path: &Path, name: &str) -> Result<(File, PathBuf), FileSystemError>;
}

pub struct LocalFolder {}

impl LocalFolder {
    pub fn new() -> Self {
        Self {}
    }
}

impl Folder for LocalFolder {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        Ok(path.canonicalize()?)
    }

    fn create_recursively(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        create_dir_all(&path)?;
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

    fn exists(&self, path: &Path) -> bool {
        path.exists() && path.is_dir()
    }

    fn pop(&self, path: &Path) -> Option<String> {
        if let Some(name) = path.file_name() {
            Some(name.to_string_lossy().to_string())
        } else {
            None
        }
    }

    fn executable_root(&self) -> Result<PathBuf, FileSystemError> {
        let exe_path = std::env::current_exe()?;
        let path = exe_path.as_path();

        match path.parent() {
            Some(path) => Ok(path.to_path_buf()),
            None => Err(FileSystemError::ParentNotFoundError(exe_path)),
        }
    }

    fn create_file(&self, path: &Path, name: &str) -> Result<(File, PathBuf), FileSystemError> {
        self.create_recursively(&path)?;
        let mut path = path.to_path_buf();
        path.push(name);

        let file = File::create(path.clone())?;
        Ok((file, path))
    }
}

#[cfg(feature = "mock")]
impl MockFolder {
    pub fn given_folder(&mut self, path: &str, exists: bool) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_exists()
            .with(predicate::eq(path.clone()))
            .return_const(exists);

        self
    }

    pub fn given_folder_exists(&mut self, path: &str) -> &mut Self {
        self.given_folder(path, true)
    }

    pub fn given_folder_does_not_exist(&mut self, path: &str) -> &mut Self {
        self.given_folder(path, false)
    }

    pub fn given_folder_entries(&mut self, path: &str, entries: Vec<Entry>) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_entries()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(entries.clone()));

        self
    }

    pub fn expect_create_folder_recursively_with(&mut self, path: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_create_recursively()
            .with(predicate::eq(path.clone()))
            .returning(|p| Ok(p.to_path_buf()));

        self
    }

    pub fn expect_pop_with(&mut self, path: &str, result: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let result = result.to_string();

        self.expect_pop()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Some(result.to_string()));

        self
    }

    pub fn given_executable_root(&mut self, path: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_executable_root()
            .returning(move || Ok(path.clone()));

        self
    }
}

#[cfg(feature = "mock")]
impl Entry {
    pub fn create_file_entry(name: &str) -> Self {
        Self {
            is_link: false,
            kind: EntryKind::File,
            name: name.to_string(),
        }
    }

    pub fn create_directory(name: &str) -> Self {
        Self {
            is_link: false,
            kind: EntryKind::Directory,
            name: name.to_string(),
        }
    }
}
