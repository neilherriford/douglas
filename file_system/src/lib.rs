pub mod encoding;
use async_trait::async_trait;
#[cfg(feature = "mock")]
use mockall::predicate;
use nix::sys::stat::{Mode, stat, umask};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::fs::{
    File, Metadata, Permissions as Perms, create_dir_all, metadata, read_dir, read_link,
    read_to_string, remove_file, rename, set_permissions,
};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, chown, symlink};
use std::path::{Path, PathBuf};
use thiserror::Error;
use users::{get_group_by_name, get_user_by_name};
use utils::ClientErrorDisplay;

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
    #[error("System error: {0}")]
    SystemError(String),
    #[error("String not representable in UTF8: {0}")]
    NonUtfString(String),
}

impl ClientErrorDisplay for FileSystemError {
    fn to_client_string(&self) -> String {
        "Could not create mount".to_string()
    }
}

impl PartialEq for FileSystemError {
    fn eq(&self, other: &Self) -> bool {
        match self {
            FileSystemError::IoError(left) => {
                if let FileSystemError::IoError(right) = other {
                    left.to_string() == right.to_string()
                } else {
                    false
                }
            }
            _ => self == other,
        }
    }
}

pub fn path_to_string<T: AsRef<Path>>(path: T) -> String {
    path.as_ref()
        .to_str()
        .unwrap_or("<Invalid path>")
        .to_string()
}

#[repr(u16)]
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Masks {
    ExcludeUser,
    Other(u16),
}

impl From<Masks> for nix::libc::mode_t {
    fn from(value: Masks) -> Self {
        match value {
            Masks::ExcludeUser => 0o007 as nix::libc::mode_t,
            Masks::Other(v) => v as nix::libc::mode_t,
        }
    }
}

impl From<Masks> for Mode {
    fn from(value: Masks) -> Self {
        Mode::from_bits_truncate(value.into())
    }
}

impl std::fmt::Display for Masks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mask: nix::libc::mode_t = self.to_owned().into();
        write!(f, "Mask 0o{mask:o}")
    }
}

#[repr(u32)]
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Modes {
    None,
    OwnerReadWrite,
    OwnerReadWriteGroupRead,
    OwnerReadWriteGroupReadWrite,
    OwnerReadWriteExecuteGroupReadWriteExecute,
    InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
    Other(u32),
}

impl From<Modes> for u32 {
    fn from(value: Modes) -> Self {
        match value {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerReadWriteGroupRead => 0o640,
            Modes::OwnerReadWriteGroupReadWrite => 0o660,
            Modes::OwnerReadWriteExecuteGroupReadWriteExecute => 0o770,
            Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute => 0o2770,
            Modes::Other(v) => v,
        }
    }
}

impl std::fmt::Display for Modes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode: u32 = self.to_owned().into();
        write!(f, "0o{mode:o}")
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
pub trait FileWriter: Send + Sync {
    fn write_all(&self, path: &Path, contents: &str) -> Result<(), FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileReader: Send + Sync {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
}

#[cfg(feature = "mock")]
impl MockFileReader {
    pub fn given_can_read_all_with_contents(&mut self, path: &str, contents: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let contents = contents.to_string();

        self.expect_read_all()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(contents.clone()));

        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileDeleter: Send + Sync {
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

#[derive(Default)]
pub struct UnixFileWriter {}

impl UnixFileWriter {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileWriter for UnixFileWriter {
    fn write_all(&self, path: &Path, contents: &str) -> Result<(), FileSystemError> {
        let mut file = File::create(path)?;
        file.write_all(contents.as_bytes())?;

        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileAppender: Send + Sync {
    fn append(&self, path: &Path, contents: String) -> Result<(), FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
}

#[derive(Default)]
pub struct UnixFileAppender {}

impl UnixFileAppender {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileAppender for UnixFileAppender {
    fn append(&self, path: &Path, contents: String) -> Result<(), FileSystemError> {
        if let Some(parent) = path.parent()
            && !self.exists(parent)
        {
            return Err(FileSystemError::ParentNotFoundError(parent.to_path_buf()));
        }

        let previous_umask = umask(Masks::ExcludeUser.into());
        let open_result = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(Modes::OwnerReadWriteGroupReadWrite.into())
            .open(path);
        umask(previous_umask);
        let mut file = open_result?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[derive(Default)]
pub struct UnixFileReader {}

impl UnixFileReader {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileReader for UnixFileReader {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError> {
        Ok(read_to_string(path)?)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[derive(Default)]
pub struct UnixFileDeleter {}

impl UnixFileDeleter {
    pub fn new() -> Self {
        Self {}
    }
}

impl FileDeleter for UnixFileDeleter {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError> {
        if path.exists() {
            remove_file(path)?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct UnixFileRenamer {}

impl UnixFileRenamer {
    pub fn new() -> Self {
        Self {}
    }
}
impl FileRenamer for UnixFileRenamer {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
        Ok(rename(from, to)?)
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Permissions: Send + Sync {
    fn change_user_and_group_ownership(
        &self,
        path: &Path,
        user_name: &str,
        group_name: &str,
    ) -> Result<(), FileSystemError>;
    fn change_mode(&self, path: &Path, mode: &Modes) -> Result<(), FileSystemError>;
    fn get_user_and_group_ownership(
        &self,
        path: &Path,
    ) -> Result<(String, String), FileSystemError>;
    fn get_mode(&self, path: &Path) -> Result<Modes, FileSystemError>;
}

#[derive(Default)]
pub struct UnixPermissions {}

impl UnixPermissions {
    pub fn new() -> Self {
        Self {}
    }

    fn try_to_string(
        name: &std::ffi::OsStr,
        name_kind: &str,
        id: &u32,
    ) -> Result<String, FileSystemError> {
        if let Some(name) = name.to_str() {
            Ok(name.to_string())
        } else {
            Err(FileSystemError::NonUtfString(format!(
                "{name_kind} for {id}",
            )))
        }
    }
}

impl Permissions for UnixPermissions {
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
        let result = set_permissions(path, Perms::from_mode(mode.to_owned().into()));
        Ok(result?)
    }

    fn get_user_and_group_ownership(
        &self,
        path: &Path,
    ) -> Result<(String, String), FileSystemError> {
        let stat_result = match stat(path) {
            Ok(stat_result) => stat_result,
            Err(errno) => return Err(FileSystemError::SystemError(errno.to_string())),
        };

        let user_name = match users::get_user_by_uid(stat_result.st_uid) {
            Some(user) => Self::try_to_string(user.name(), "user name", &stat_result.st_uid)?,
            None => {
                return Err(FileSystemError::UserNotFoundError(
                    stat_result.st_uid.to_string(),
                ));
            }
        };
        let group_name = match users::get_group_by_gid(stat_result.st_gid) {
            Some(group) => Self::try_to_string(group.name(), "group name", &stat_result.st_gid)?,
            None => {
                return Err(FileSystemError::UserNotFoundError(
                    stat_result.st_uid.to_string(),
                ));
            }
        };

        Ok((user_name, group_name))
    }

    fn get_mode(&self, path: &Path) -> Result<Modes, FileSystemError> {
        let stat_result = match stat(path) {
            Ok(stat_result) => stat_result,
            Err(errno) => return Err(FileSystemError::SystemError(errno.to_string())),
        };

        // st_mode includes file-type bits in the high nibble (e.g. 0o40000 for
        // directories). Mask them off so we only compare permission bits.
        Ok(match stat_result.st_mode & 0o7777 {
            0 => Modes::None,
            0o600 => Modes::OwnerReadWrite,
            0o640 => Modes::OwnerReadWriteGroupRead,
            0o660 => Modes::OwnerReadWriteGroupReadWrite,
            0o770 => Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            0o2770 => Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
            other => Modes::Other(other as u32),
        })
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
pub trait BindableUnixDomainSocketFile {
    fn bind(
        &self,
        path: &Path,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, FileSystemError>;
}

#[derive(Default)]
pub struct UnixDomainSocket {}

impl UnixDomainSocket {
    pub fn new() -> Self {
        Self {}
    }
}

impl BindableUnixDomainSocketFile for UnixDomainSocket {
    fn bind(
        &self,
        path: &Path,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, FileSystemError> {
        Ok(Box::new(tokio::net::UnixListener::bind(path)?))
    }
}

#[cfg(feature = "mock")]
impl MockBindableUnixDomainSocketFile {
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
pub trait Links: Send + Sync {
    fn create(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
    fn read(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
}

#[derive(Default)]
pub struct UnixLinks {}

impl UnixLinks {
    pub fn new() -> Self {
        Self {}
    }
}

impl Links for UnixLinks {
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

#[derive(Default)]
pub struct UnixInspect {}

impl UnixInspect {
    pub fn new() -> Self {
        Self {}
    }
}

impl Inspect for UnixInspect {
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
pub trait Folder: Send + Sync {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn create_recursively(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn entries(&self, path: &Path) -> Result<Vec<Entry>, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
    fn pop(&self, path: &Path) -> Option<String>;
    fn executable_root(&self) -> Result<PathBuf, FileSystemError>;
    fn create_file(&self, path: &Path) -> Result<File, FileSystemError>;
    fn open_file_for_writing(&self, path: &Path) -> Result<File, FileSystemError>;
    fn parent(&self, path: &Path) -> Option<PathBuf>;
    fn combine(&self, left: &Path, right: &Path) -> PathBuf;
    fn split(&self, path: &Path) -> Vec<PathBuf>;
}

#[derive(Default)]
pub struct UnixFolder {}

impl UnixFolder {
    pub fn new() -> Self {
        Self {}
    }
}

impl Folder for UnixFolder {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        Ok(path.canonicalize()?)
    }

    fn create_recursively(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        create_dir_all(path)?;

        self.canonicalize(path)
    }

    fn entries(&self, path: &Path) -> Result<Vec<Entry>, FileSystemError> {
        let mut result = Vec::<Entry>::new();

        result.extend(read_dir(path)?.filter_map(|entry| {
            let entry = entry.ok()?;

            Entry::try_create_entry(entry.file_name(), entry.metadata()).ok()
        }));

        Ok(result)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists() && path.is_dir()
    }

    fn pop(&self, path: &Path) -> Option<String> {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
    }

    fn parent(&self, path: &Path) -> Option<PathBuf> {
        path.parent().map(|parent| parent.to_path_buf())
    }

    fn executable_root(&self) -> Result<PathBuf, FileSystemError> {
        let exe_path = std::env::current_exe()?;
        let path = exe_path.as_path();

        match path.parent() {
            Some(path) => Ok(path.to_path_buf()),
            None => Err(FileSystemError::ParentNotFoundError(exe_path)),
        }
    }

    fn create_file(&self, path: &Path) -> Result<File, FileSystemError> {
        let parent = match self.parent(path) {
            Some(parent) => parent,
            None => return Err(FileSystemError::ParentNotFoundError(path.to_path_buf())),
        };
        self.create_recursively(&parent)?;

        let file = File::create(path)?;
        Ok(file)
    }

    fn combine(&self, left: &Path, right: &Path) -> PathBuf {
        left.join(right).to_path_buf()
    }

    fn split(&self, path: &Path) -> Vec<PathBuf> {
        path.components()
            .filter_map(|component| {
                if let std::path::Component::Normal(os_str) = component {
                    Some(PathBuf::from(os_str))
                } else {
                    None
                }
            })
            .collect()
    }

    fn open_file_for_writing(&self, path: &Path) -> Result<File, FileSystemError> {
        let result = OpenOptions::new().create(false).append(true).open(path)?;
        Ok(result)
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

    pub fn given_exists(&mut self, path: &str) -> &mut Self {
        self.given_folder(path, true)
    }

    pub fn given_does_not_exist(&mut self, path: &str) -> &mut Self {
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

#[cfg(feature = "mock")]
impl MockFileWriter {
    pub fn expect_write_to_file_with_something(&mut self, path: &str) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_write_all()
            .with(predicate::eq(path.clone()), predicate::always())
            .returning(|_, _| Ok(()));
        self
    }
}
