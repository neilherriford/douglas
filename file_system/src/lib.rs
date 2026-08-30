pub mod encoding;
use async_trait::async_trait;
#[cfg(feature = "mock")]
use mockall::predicate;
use nix::sys::stat::{Mode, stat, umask};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{
    File, Metadata, Permissions as Perms, create_dir_all, read, read_dir, read_link,
    read_to_string, remove_dir_all, remove_file, rename, set_permissions, symlink_metadata,
};
use std::fs::{OpenOptions, hard_link};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, chown, symlink};
use std::path::{Component, Components, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use thiserror::Error;
use tokio::io::AsyncRead;
use users::{get_group_by_name, get_user_by_name};
use utils::ClientErrorDisplay;

#[derive(Debug, Error)]
pub enum RelativePathError {
    #[error("Name cannot be empty")]
    CannotBeEmpty,
    #[error("Name too long")]
    TooLong,
    #[error("Name is invalid")]
    InvalidName,
    #[error("Must be UTF-8")]
    NotUtf8,
    #[error("Invalid segment: '{0}'")]
    InvalidSegment(String),
    #[error("Not relative path")]
    NotRelative(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath {
    value: PathBuf,
}

impl RelativePath {
    pub fn root() -> Self {
        Self {
            value: PathBuf::new(),
        }
    }
}

impl Serialize for RelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.value.to_string_lossy())
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        RelativePath::try_from(PathBuf::from(value)).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for RelativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value.display())
    }
}

impl TryFrom<PathBuf> for RelativePath {
    type Error = RelativePathError;

    fn try_from(value: PathBuf) -> Result<Self, Self::Error> {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9]+(?:[._-][a-zA-Z0-9]+)*$").unwrap());

        for component in value.components() {
            match component {
                std::path::Component::Normal(segment) => {
                    let segment = segment.to_str().ok_or(RelativePathError::NotUtf8)?;
                    if !PATTERN.is_match(segment) {
                        return Err(RelativePathError::InvalidSegment(segment.to_string()));
                    }
                }
                _ => return Err(RelativePathError::NotRelative(value)),
            }
        }
        Ok(Self { value })
    }
}

impl From<RelativePath> for PathBuf {
    fn from(value: RelativePath) -> Self {
        value.value
    }
}

impl AsRef<Path> for RelativePath {
    fn as_ref(&self) -> &Path {
        self.value.as_ref()
    }
}

#[derive(Error, Debug)]
pub enum FileSystemError {
    #[error("Parent not found for path: {0}")]
    ParentNotFoundError(PathBuf),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("IO error at '{path}'")]
    IoErrorAtPath {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
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
    #[error("Given child {child} is not a child of the root folder {root}")]
    NotChild { root: PathBuf, child: PathBuf },
    #[error("Expected normalized path {0}")]
    ExpectedNormalizedPath(PathBuf),
    #[error("Invalid path {0}")]
    InvalidPath(PathBuf),
}

impl ClientErrorDisplay for FileSystemError {
    fn to_client_string(&self) -> String {
        "Could not create mount".to_string()
    }
}

impl PartialEq for FileSystemError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                FileSystemError::ParentNotFoundError(left),
                FileSystemError::ParentNotFoundError(right),
            ) => left == right,
            (FileSystemError::IoError(left), FileSystemError::IoError(right)) => {
                left.to_string() == right.to_string()
            }
            (
                FileSystemError::IoErrorAtPath {
                    path: left_path,
                    error: left_error,
                },
                FileSystemError::IoErrorAtPath {
                    path: right_path,
                    error: right_error,
                },
            ) => left_path == right_path && left_error.to_string() == right_error.to_string(),
            (FileSystemError::NotFoundError(left), FileSystemError::NotFoundError(right)) => {
                left == right
            }
            (
                FileSystemError::GroupNotFoundError(left),
                FileSystemError::GroupNotFoundError(right),
            ) => left == right,
            (
                FileSystemError::UserNotFoundError(left),
                FileSystemError::UserNotFoundError(right),
            ) => left == right,
            (
                FileSystemError::InvalidFileNameError(left),
                FileSystemError::InvalidFileNameError(right),
            ) => left == right,
            (FileSystemError::ExpectedFileError, FileSystemError::ExpectedFileError) => true,
            (FileSystemError::SystemError(left), FileSystemError::SystemError(right)) => {
                left == right
            }
            (FileSystemError::NonUtfString(left), FileSystemError::NonUtfString(right)) => {
                left == right
            }
            (
                FileSystemError::NotChild {
                    root: left_root,
                    child: left_child,
                },
                FileSystemError::NotChild {
                    root: right_root,
                    child: right_child,
                },
            ) => left_root == right_root && left_child == right_child,
            (
                FileSystemError::ExpectedNormalizedPath(left),
                FileSystemError::ExpectedNormalizedPath(right),
            ) => left == right,
            (FileSystemError::InvalidPath(left), FileSystemError::InvalidPath(right)) => {
                left == right
            }
            _ => false,
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
    OwnerReadWriteExecute,
    OwnerReadWriteGroupRead,
    OwnerReadWriteGroupReadWrite,
    OwnerReadWriteExecuteGroupReadWriteExecute,
    InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
    OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
    InheritedOwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
    Other(u32),
}

impl From<Modes> for u32 {
    fn from(value: Modes) -> Self {
        match value {
            Modes::None => 0,
            Modes::OwnerReadWrite => 0o600,
            Modes::OwnerReadWriteExecute => 0o700,
            Modes::OwnerReadWriteGroupRead => 0o640,
            Modes::OwnerReadWriteGroupReadWrite => 0o660,
            Modes::OwnerReadWriteExecuteGroupReadWriteExecute => 0o770,
            Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute => 0o2770,
            Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute => 0o771,
            Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute => 0o2771,
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
    pub path: PathBuf,
    pub kind: EntryKind,
    pub is_link: bool,
    pub size: u64,
}

impl Entry {
    fn try_create_entry(
        parent: &Path,
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
        let is_symlink;
        let size;

        match metadata {
            Ok(metadata) => {
                if metadata.is_file() {
                    kind = EntryKind::File;
                } else {
                    kind = EntryKind::Directory;
                }
                is_symlink = metadata.is_symlink();
                size = metadata.len();
            }
            Err(err) => return Err(FileSystemError::IoError(err)),
        }

        let mut path = parent.to_path_buf();
        path.push(&entry_name);

        Ok(Entry {
            path,
            name: entry_name,
            kind,
            is_link: is_symlink,
            size,
        })
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait BufferedFileWiter: Send {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), FileSystemError>;
    fn close(&mut self);
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileWriter: Send + Sync {
    fn write_all(&self, path: &Path, contents: &str) -> Result<(), FileSystemError>;
    fn write_all_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), FileSystemError>;
    /// Atomic create-if-missing: `Ok(true)` if it wrote, `Ok(false)` if the file already existed.
    fn write_all_bytes_if_absent(&self, path: &Path, bytes: &[u8])
    -> Result<bool, FileSystemError>;
    fn create_buffered_file_writer(
        &self,
        path: &Path,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Result<Box<dyn BufferedFileWiter>, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileReader: Send + Sync {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError>;
    fn read_all_bytes(&self, path: &Path) -> Result<Vec<u8>, FileSystemError>;
    fn read_stdin(&self) -> Result<String, FileSystemError>;
    fn exists(&self, path: &Path) -> bool;
    fn create_reader(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, FileSystemError>;
}

#[cfg(feature = "mock")]
impl MockFileReader {
    pub fn given_file(&mut self, path: &str, exists: bool) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_exists()
            .with(predicate::eq(path.clone()))
            .return_const(exists);

        self
    }

    pub fn given_exists(&mut self, path: &str) -> &mut Self {
        self.given_file(path, true)
    }

    pub fn given_does_not_exist(&mut self, path: &str) -> &mut Self {
        self.given_file(path, false)
    }

    pub fn given_can_read_all_with_contents(&mut self, path: &str, contents: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let contents = contents.to_string();

        self.expect_read_all()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(contents.clone()));

        self
    }

    pub fn given_can_read_all_bytes_with_contents(
        &mut self,
        path: &str,
        contents: &[u8],
    ) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let contents = contents.to_vec();

        self.expect_read_all_bytes()
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

    pub fn given_delete_to_fail_once_with(
        &mut self,
        path: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_delete()
            .with(predicate::eq(path.clone()))
            .return_once(move |_| Err(error));
        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FolderDeleter: Send + Sync {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError>;
}

#[cfg(feature = "mock")]
impl MockFolderDeleter {
    pub fn expect_folder_to_be_deleted(&mut self, path: &str) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_delete()
            .with(predicate::eq(path.clone()))
            .returning(|_| Ok(()));
        self
    }

    pub fn given_delete_to_fail_once_with(
        &mut self,
        path: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_delete()
            .with(predicate::eq(path.clone()))
            .return_once(move |_| Err(error));
        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileRenamer: Send + Sync {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
}

struct UnixBufferedFileWriter {
    file: File,
    closed: bool,
    path: PathBuf,
    file_deleter: Arc<dyn FileDeleter>,
}

impl UnixBufferedFileWriter {
    pub fn build(
        path: PathBuf,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Result<Self, std::io::Error> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            file,
            closed: false,
            path,
            file_deleter,
        })
    }
}

impl Drop for UnixBufferedFileWriter {
    fn drop(&mut self) {
        let _ = self.file.flush();
        if !self.closed {
            let _ = self.file_deleter.delete(self.path.as_path());
            self.closed = true;
        }
    }
}

impl BufferedFileWiter for UnixBufferedFileWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), FileSystemError> {
        self.file.write_all(bytes)?;
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true
    }
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
        self.write_all_bytes(path, contents.as_bytes())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists() && !path.is_dir()
    }

    fn write_all_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), FileSystemError> {
        let mut file = File::create(path).map_err(|error| FileSystemError::IoErrorAtPath {
            path: path.to_path_buf(),
            error,
        })?;
        file.write_all(bytes.as_ref())
            .map_err(|error| FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            })?;

        Ok(())
    }

    fn write_all_bytes_if_absent(
        &self,
        path: &Path,
        bytes: &[u8],
    ) -> Result<bool, FileSystemError> {
        let file = OpenOptions::new().write(true).create_new(true).open(path);

        let mut file = match file {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => {
                return Err(FileSystemError::IoErrorAtPath {
                    path: path.to_path_buf(),
                    error,
                });
            }
        };

        file.write_all(bytes)
            .map_err(|error| FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            })?;

        Ok(true)
    }

    fn create_buffered_file_writer(
        &self,
        path: &Path,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Result<Box<dyn BufferedFileWiter>, FileSystemError> {
        let result: Box<dyn BufferedFileWiter> = Box::new(UnixBufferedFileWriter::build(
            path.to_path_buf(),
            file_deleter,
        )?);
        Ok(result)
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait FileAppender: Send + Sync {
    fn append(&self, path: &Path, contents: String) -> Result<(), FileSystemError>;
    fn append_all_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), FileSystemError>;
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
        self.append_all_bytes(path, contents.as_bytes())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists() && !path.is_dir()
    }

    fn append_all_bytes(&self, path: &Path, bytes: &[u8]) -> Result<(), FileSystemError> {
        if let Some(parent) = path.parent()
            && !parent.is_dir()
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
        let mut file = open_result.map_err(|error| FileSystemError::IoErrorAtPath {
            path: path.to_path_buf(),
            error,
        })?;
        file.write_all(bytes)
            .map_err(|error| FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            })?;
        Ok(())
    }
}

#[derive(Default)]
pub struct UnixFileReader {}

impl UnixFileReader {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl FileReader for UnixFileReader {
    fn read_all(&self, path: &Path) -> Result<String, FileSystemError> {
        read_to_string(path).map_err(|error| FileSystemError::IoErrorAtPath {
            path: path.to_path_buf(),
            error,
        })
    }

    fn read_all_bytes(&self, path: &Path) -> Result<Vec<u8>, FileSystemError> {
        read(path).map_err(|error| FileSystemError::IoErrorAtPath {
            path: path.to_path_buf(),
            error,
        })
    }

    fn read_stdin(&self) -> Result<String, FileSystemError> {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        Ok(input)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists() && !path.is_dir()
    }

    fn create_reader(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, FileSystemError> {
        if !self.exists(path) {
            return Err(FileSystemError::NotFoundError(path.to_path_buf()));
        }

        let file = tokio::fs::File::from_std(std::fs::File::open(path).map_err(|error| {
            FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            }
        })?);
        Ok(Box::new(file))
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
            remove_file(path).map_err(|error| FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            })?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct UnixFolderDeleter {}

impl UnixFolderDeleter {
    pub fn new() -> Self {
        Self {}
    }
}

impl FolderDeleter for UnixFolderDeleter {
    fn delete(&self, path: &Path) -> Result<(), FileSystemError> {
        if path.exists() {
            remove_dir_all(path).map_err(|error| FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error,
            })?;
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
        rename(from, to).map_err(|error| FileSystemError::IoErrorAtPath {
            path: from.to_path_buf(),
            error,
        })
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
            0o771 => Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            0o2771 => Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
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

    pub fn given_ownership_and_mode(
        &mut self,
        path: &str,
        user: &str,
        group: &str,
        mode: Modes,
    ) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);
        let user = user.to_string();
        let group = group.to_string();

        self.expect_get_user_and_group_ownership()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok((user.clone(), group.clone())));

        self.expect_get_mode()
            .with(predicate::eq(path))
            .returning(move |_| Ok(mode));

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
pub trait BindableUnixDomainSocketFile: Send + Sync {
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
    fn create_symbolic(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
    fn follow_symbolic(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn create_hard(&self, from: &Path, to: &Path) -> Result<(), FileSystemError>;
}

#[derive(Default)]
pub struct UnixLinks {}

impl UnixLinks {
    pub fn new() -> Self {
        Self {}
    }
}

impl Links for UnixLinks {
    fn create_symbolic(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
        Ok(symlink(from, to)?)
    }

    fn follow_symbolic(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
        Ok(read_link(path)?)
    }

    fn create_hard(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
        Ok(hard_link(from, to)?)
    }
}

#[cfg(feature = "mock")]
impl MockLinks {
    pub fn given_symlink(&mut self, from: &str, to: &str) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_follow_symbolic()
            .with(predicate::eq(from.clone()))
            .returning(move |_| Ok(to.clone()));

        self
    }

    pub fn expect_create_symbolic_with(&mut self, from: &str, to: &str) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_create_symbolic()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .returning(|_, _| Ok(()));

        self
    }

    pub fn expect_create_hard_with(&mut self, from: &str, to: &str) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_create_hard()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .returning(|_, _| Ok(()));

        self
    }

    pub fn given_create_hard_fails_once_with(
        &mut self,
        from: &str,
        to: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let from = from.to_string();
        let from = PathBuf::from(from);

        let to = to.to_string();
        let to = PathBuf::from(to);

        self.expect_create_hard()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .return_once(move |_, _| Err(error));

        self
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Inspect: Send + Sync {
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
        let mut parent = path.to_path_buf();
        parent.pop();

        Entry::try_create_entry(&parent, name, symlink_metadata(path))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Folder: Send + Sync {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError>;
    fn create_relative_path(
        &self,
        root: &Path,
        child: &Path,
    ) -> Result<RelativePath, FileSystemError>;
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

    fn assert_normalized<'a>(&self, path: &'a Path) -> Result<Components<'a>, FileSystemError> {
        for component in path.components() {
            if matches!(component, Component::ParentDir) || matches!(component, Component::CurDir) {
                return Err(FileSystemError::ExpectedNormalizedPath(path.to_path_buf()));
            }
        }
        Ok(path.components())
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

            Entry::try_create_entry(path, entry.file_name(), entry.metadata()).ok()
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

    fn create_relative_path(
        &self,
        root: &Path,
        child: &Path,
    ) -> Result<RelativePath, FileSystemError> {
        let root_components: Vec<std::path::Component> = self.assert_normalized(root)?.collect();
        let mut child_components: VecDeque<std::path::Component> =
            self.assert_normalized(child)?.collect();

        if child_components.len() < root_components.len() {
            return Err(FileSystemError::NotChild {
                root: root.to_path_buf(),
                child: child.to_path_buf(),
            });
        }

        for root_component in root_components {
            let child_component = if let Some(child_components) = child_components.pop_front() {
                child_components
            } else {
                return Err(FileSystemError::InvalidPath(child.to_path_buf()));
            };

            if root_component != child_component {
                return Err(FileSystemError::NotChild {
                    root: root.to_path_buf(),
                    child: child.to_path_buf(),
                });
            }
        }

        let mut result = PathBuf::new();
        for child_component in child_components {
            result.push(child_component);
        }

        RelativePath::try_from(result)
            .map_err(|_| FileSystemError::InvalidPath(child.to_path_buf()))
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

    pub fn given_relative_path_with(
        &mut self,
        root: &str,
        child: &str,
        relative: &str,
    ) -> &mut Self {
        let root = PathBuf::from(root);
        let child = PathBuf::from(child);
        let relative = RelativePath::try_from(PathBuf::from(relative))
            .expect("expected a valid relative path");

        self.expect_create_relative_path()
            .with(predicate::eq(root), predicate::eq(child))
            .returning(move |_, _| Ok(relative.clone()));

        self
    }

    pub fn given_relative_path_to_fail_once_with(
        &mut self,
        root: &str,
        child: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let root = PathBuf::from(root);
        let child = PathBuf::from(child);

        self.expect_create_relative_path()
            .with(predicate::eq(root), predicate::eq(child))
            .return_once(move |_, _| Err(error));

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
        Self::create_file_entry_in("", name)
    }

    pub fn create_directory(name: &str) -> Self {
        Self::create_directory_in("", name)
    }

    pub fn create_file_entry_in(parent: &str, name: &str) -> Self {
        Self {
            path: Path::new(parent).join(name),
            is_link: false,
            kind: EntryKind::File,
            name: name.to_string(),
            size: 123,
        }
    }

    pub fn create_directory_in(parent: &str, name: &str) -> Self {
        Self {
            path: Path::new(parent).join(name),
            is_link: false,
            kind: EntryKind::Directory,
            name: name.to_string(),
            size: 0,
        }
    }

    pub fn create_symlink_in(parent: &str, name: &str) -> Self {
        Self {
            path: Path::new(parent).join(name),
            is_link: true,
            kind: EntryKind::File,
            name: name.to_string(),
            size: 0,
        }
    }
}

#[cfg(feature = "mock")]
impl MockFileWriter {
    pub fn expect_write_to_file_with_contents(&mut self, path: &str, contents: &str) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_write_all()
            .with(
                predicate::eq(path.clone()),
                predicate::eq(contents.to_string()),
            )
            .returning(|_, _| Ok(()));
        self
    }

    pub fn given_write_to_file_fails_once_with(
        &mut self,
        path: &str,
        contents: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_write_all()
            .with(
                predicate::eq(path.clone()),
                predicate::eq(contents.to_string()),
            )
            .return_once(move |_, _| Err(error));
        self
    }
}

#[cfg(feature = "mock")]
impl MockInspect {
    pub fn given_entry(&mut self, path: &str, exists: bool) -> &mut Self {
        let path = path.to_string();
        let path = PathBuf::from(path);

        self.expect_exists()
            .with(predicate::eq(path.clone()))
            .return_const(exists);

        self
    }

    pub fn given_exists(&mut self, path: &str) -> &mut Self {
        self.given_entry(path, true)
    }

    pub fn given_does_not_exist(&mut self, path: &str) -> &mut Self {
        self.given_entry(path, false)
    }

    pub fn expect_entry_with_metadata(&mut self, path: &str, entry: Entry) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_read_metadata()
            .with(predicate::eq(path.clone()))
            .returning(move |_| Ok(entry.clone()));
        self
    }
}

#[cfg(feature = "mock")]
impl MockFileAppender {
    pub fn expect_append_all_bytes_with(&mut self, path: &str, bytes: Vec<u8>) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_append_all_bytes()
            .with(
                predicate::eq(path.clone()),
                predicate::function(move |b: &[u8]| b == bytes.clone()),
            )
            .returning(|_, _| Ok(()));
        self
    }

    pub fn given_append_all_bytes_fails_once_with(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        error: FileSystemError,
    ) -> &mut Self {
        let path = Path::new(path);
        let path = path.to_path_buf();

        self.expect_append_all_bytes()
            .with(
                predicate::eq(path.clone()),
                predicate::function(move |b: &[u8]| b == bytes.clone()),
            )
            .return_once(move |_, _| Err(error));
        self
    }
}

#[cfg(feature = "mock")]
impl MockFileRenamer {
    pub fn expect_rename_with(&mut self, from: &str, to: &str) -> &mut Self {
        let from = Path::new(from);
        let from = from.to_path_buf();

        let to = Path::new(to);
        let to = to.to_path_buf();

        self.expect_rename()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .returning(|_, _| Ok(()));
        self
    }

    pub fn given_rename_fails_once_with(
        &mut self,
        from: &str,
        to: &str,
        error: FileSystemError,
    ) -> &mut Self {
        let from = Path::new(from);
        let from = from.to_path_buf();

        let to = Path::new(to);
        let to = to.to_path_buf();

        self.expect_rename()
            .with(predicate::eq(from.clone()), predicate::eq(to.clone()))
            .return_once(move |_, _| Err(error));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod relative_path {
        use super::*;

        #[test]
        fn test_try_from_should_accept_a_single_segment() {
            let result = RelativePath::try_from(PathBuf::from("static.yml"));

            assert!(result.is_ok());
        }

        #[test]
        fn test_try_from_should_accept_nested_segments() {
            let result = RelativePath::try_from(PathBuf::from("config/dynamic"));

            assert!(result.is_ok());
        }

        #[test]
        fn test_try_from_should_accept_dots_underscores_and_dashes_within_a_segment() {
            let result = RelativePath::try_from(PathBuf::from("traefik-dynamic_v2.yml"));

            assert!(result.is_ok());
        }

        #[test]
        fn test_try_from_should_reject_parent_dir_traversal() {
            let result = RelativePath::try_from(PathBuf::from("../../etc/passwd"));

            assert!(matches!(result, Err(RelativePathError::NotRelative(_))));
        }

        #[test]
        fn test_try_from_should_reject_absolute_paths() {
            let result = RelativePath::try_from(PathBuf::from("/etc/passwd"));

            assert!(matches!(result, Err(RelativePathError::NotRelative(_))));
        }

        #[test]
        fn test_try_from_should_reject_a_bare_dot_segment() {
            let result = RelativePath::try_from(PathBuf::from("./config"));

            assert!(matches!(result, Err(RelativePathError::NotRelative(_))));
        }

        #[test]
        fn test_try_from_should_reject_invalid_characters_in_a_segment() {
            let result = RelativePath::try_from(PathBuf::from("has spaces"));

            assert!(matches!(result, Err(RelativePathError::InvalidSegment(_))));
        }

        #[test]
        fn test_try_from_should_reject_consecutive_separators_in_a_segment() {
            let result = RelativePath::try_from(PathBuf::from("foo..bar"));

            assert!(matches!(result, Err(RelativePathError::InvalidSegment(_))));
        }

        #[test]
        fn test_root_should_produce_an_empty_relative_path() {
            let root = RelativePath::root();

            assert_eq!(PathBuf::from(root), PathBuf::new());
        }

        #[test]
        fn test_as_ref_should_expose_the_underlying_path() {
            let relative = RelativePath::try_from(PathBuf::from("config/static.yml"))
                .expect("should be valid");

            assert_eq!(relative.as_ref(), Path::new("config/static.yml"));
        }
    }

    mod create_relative_path {
        use super::*;

        #[test]
        fn test_should_strip_the_root_prefix_from_the_child() {
            let folder = UnixFolder::new();

            let result = folder
                .create_relative_path(
                    Path::new("/var/lib/seedbank/seeds/foo"),
                    Path::new("/var/lib/seedbank/seeds/foo/config/static.yml"),
                )
                .expect("should be a child path");

            assert_eq!(PathBuf::from(result), PathBuf::from("config/static.yml"));
        }

        #[test]
        fn test_should_succeed_with_an_empty_relative_path_when_child_equals_root() {
            let folder = UnixFolder::new();

            let result = folder
                .create_relative_path(
                    Path::new("/var/lib/seedbank/seeds/foo"),
                    Path::new("/var/lib/seedbank/seeds/foo"),
                )
                .expect("should be a child path");

            assert_eq!(PathBuf::from(result), PathBuf::new());
        }

        #[test]
        fn test_should_fail_when_child_is_not_under_root() {
            let folder = UnixFolder::new();

            let result = folder.create_relative_path(
                Path::new("/var/lib/seedbank/seeds/foo"),
                Path::new("/var/lib/seedbank/seeds/bar/config/static.yml"),
            );

            assert!(matches!(result, Err(FileSystemError::NotChild { .. })));
        }

        #[test]
        fn test_should_fail_when_child_is_shorter_than_root() {
            let folder = UnixFolder::new();

            let result = folder.create_relative_path(
                Path::new("/var/lib/seedbank/seeds/foo"),
                Path::new("/var/lib/seedbank"),
            );

            assert!(matches!(result, Err(FileSystemError::NotChild { .. })));
        }

        #[test]
        fn test_should_fail_when_root_is_not_normalized() {
            let folder = UnixFolder::new();

            let result = folder.create_relative_path(
                Path::new("/var/lib/seedbank/seeds/../seeds/foo"),
                Path::new("/var/lib/seedbank/seeds/foo/config/static.yml"),
            );

            assert!(matches!(
                result,
                Err(FileSystemError::ExpectedNormalizedPath(_))
            ));
        }

        #[test]
        fn test_should_fail_when_child_is_not_normalized() {
            let folder = UnixFolder::new();

            let result = folder.create_relative_path(
                Path::new("/var/lib/seedbank/seeds/foo"),
                Path::new("/var/lib/seedbank/seeds/foo/config/../static.yml"),
            );

            assert!(matches!(
                result,
                Err(FileSystemError::ExpectedNormalizedPath(_))
            ));
        }

        #[test]
        fn test_should_reject_a_child_segment_that_is_not_a_valid_relative_path_segment() {
            let folder = UnixFolder::new();

            let result = folder.create_relative_path(
                Path::new("/var/lib/seedbank/seeds/foo"),
                Path::new("/var/lib/seedbank/seeds/foo/has spaces/static.yml"),
            );

            assert!(matches!(result, Err(FileSystemError::InvalidPath(_))));
        }
    }

    mod unix_file_appender {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};

        fn unique_temp_dir() -> PathBuf {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "douglas-file-appender-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            dir
        }

        #[test]
        fn test_append_all_bytes_should_write_when_the_parent_directory_exists() {
            let dir = unique_temp_dir();
            std::fs::create_dir_all(&dir).expect("should create temp dir");
            let mut file_path = dir.clone();
            file_path.push("blob");

            let appender = UnixFileAppender::new();
            let result = appender.append_all_bytes(&file_path, b"hello");

            assert!(result.is_ok());
            assert_eq!(
                std::fs::read(&file_path).expect("should read back written bytes"),
                b"hello"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn test_append_all_bytes_should_fail_when_the_parent_directory_is_missing() {
            let dir = unique_temp_dir();
            let mut file_path = dir.clone();
            file_path.push("blob");

            let appender = UnixFileAppender::new();
            let result = appender.append_all_bytes(&file_path, b"hello");

            assert!(matches!(
                result,
                Err(FileSystemError::ParentNotFoundError(parent)) if parent == dir
            ));
        }
    }

    mod modes {
        use super::*;

        #[test]
        fn test_owner_read_write_execute_should_convert_to_0700() {
            assert_eq!(u32::from(Modes::OwnerReadWriteExecute), 0o700);
        }
    }

    mod unix_file_writer {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};

        fn unique_temp_dir() -> PathBuf {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "douglas-file-writer-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            dir
        }

        #[test]
        fn test_write_all_bytes_if_absent_should_write_and_return_true_when_missing() {
            let dir = unique_temp_dir();
            std::fs::create_dir_all(&dir).expect("should create temp dir");
            let mut file_path = dir.clone();
            file_path.push("identity");

            let writer = UnixFileWriter::new();
            let result = writer.write_all_bytes_if_absent(&file_path, b"secret");

            assert!(matches!(result, Ok(true)));
            assert_eq!(
                std::fs::read(&file_path).expect("should read back written bytes"),
                b"secret"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn test_write_all_bytes_if_absent_should_not_overwrite_and_return_false_when_present() {
            let dir = unique_temp_dir();
            std::fs::create_dir_all(&dir).expect("should create temp dir");
            let mut file_path = dir.clone();
            file_path.push("identity");
            std::fs::write(&file_path, b"original").expect("should seed existing file");

            let writer = UnixFileWriter::new();
            let result = writer.write_all_bytes_if_absent(&file_path, b"attacker-controlled");

            assert!(matches!(result, Ok(false)));
            assert_eq!(
                std::fs::read(&file_path).expect("should read back original bytes"),
                b"original"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
