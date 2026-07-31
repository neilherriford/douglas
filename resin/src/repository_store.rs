use file_system::{EntryKind, FileSystemError, Folder};
use resin_types::Name;
use std::{path::PathBuf, str::FromStr, sync::Arc};

#[cfg_attr(test, mockall::automock)]
pub trait RepositoryStore: Send + Sync {
    fn list(&self) -> Result<Vec<Name>, FileSystemError>;
}

pub struct FileRepositoryStore {
    folder: Arc<dyn Folder>,
    repository_root: PathBuf,
}

impl FileRepositoryStore {
    pub fn new(repository_root: PathBuf, folder: Arc<dyn Folder>) -> Self {
        Self {
            folder,
            repository_root,
        }
    }
}

impl RepositoryStore for FileRepositoryStore {
    fn list(&self) -> Result<Vec<Name>, FileSystemError> {
        Ok(self
            .folder
            .entries(&self.repository_root)?
            .iter()
            .filter_map(|entry| {
                if entry.kind != EntryKind::Directory {
                    return None;
                }
                Name::from_str(&entry.name).ok()
            })
            .collect())
    }
}
