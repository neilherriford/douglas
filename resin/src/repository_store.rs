use file_system::{EntryKind, FileSystemError, Folder, FolderDeleter};
use resin_types::Name;
use std::{path::PathBuf, str::FromStr, sync::Arc};

#[cfg_attr(test, mockall::automock)]
pub trait RepositoryStore: Send + Sync {
    fn list(&self) -> Result<Vec<Name>, FileSystemError>;
    fn delete(&self, name: &Name) -> Result<(), FileSystemError>;
}

pub struct FileRepositoryStore {
    folder: Arc<dyn Folder>,
    folder_deleter: Arc<dyn FolderDeleter>,
    repository_root: PathBuf,
}

impl FileRepositoryStore {
    pub fn new(
        repository_root: PathBuf,
        folder: Arc<dyn Folder>,
        folder_deleter: Arc<dyn FolderDeleter>,
    ) -> Self {
        Self {
            folder,
            folder_deleter,
            repository_root,
        }
    }

    fn repository_path(&self, name: &Name) -> PathBuf {
        let mut path = self.repository_root.clone();
        path.push(name.fs_safe());
        path
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

    fn delete(&self, name: &Name) -> Result<(), FileSystemError> {
        self.folder_deleter.delete(&self.repository_path(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFolder, MockFolderDeleter};

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("valid name")
    }

    #[test]
    fn test_delete_should_remove_the_repositorys_own_directory() {
        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter
            .expect_delete()
            .withf(|path| path.to_str() == Some("/repositories/hello-world"))
            .returning(|_| Ok(()));

        let store = FileRepositoryStore::new(
            PathBuf::from("/repositories"),
            Arc::new(MockFolder::new()),
            Arc::new(folder_deleter),
        );

        let result = store.delete(&name("hello-world"));

        assert!(result.is_ok());
    }
}
