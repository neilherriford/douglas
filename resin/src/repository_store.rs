use file_system::{EntryKind, FileSystemError, Folder, FolderDeleter};
use resin_types::{Name, Repository};
use std::{path::PathBuf, str::FromStr, sync::Arc};

#[cfg_attr(test, mockall::automock)]
pub trait RepositoryStore: Send + Sync {
    fn list(&self) -> Result<Vec<Repository>, FileSystemError>;
    fn delete(&self, repository: &Repository) -> Result<(), FileSystemError>;
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

    fn directory_entries(&self, path: &std::path::Path) -> Result<Vec<String>, FileSystemError> {
        if !self.folder.exists(path) {
            return Ok(Vec::new());
        }

        Ok(self
            .folder
            .entries(path)?
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Directory)
            .map(|entry| entry.name)
            .collect())
    }

    fn list_local(&self) -> Result<Vec<Repository>, FileSystemError> {
        let local_root = self.repository_root.join("local");

        Ok(self
            .directory_entries(&local_root)?
            .into_iter()
            .filter_map(|name| Name::from_str(&name).ok())
            .map(Repository::Local)
            .collect())
    }

    fn list_upstream(&self) -> Result<Vec<Repository>, FileSystemError> {
        let upstream_root = self.repository_root.join("upstream");
        let mut repositories = Vec::new();

        for host in self.directory_entries(&upstream_root)? {
            let host_root = upstream_root.join(&host);
            for escaped_path in self.directory_entries(&host_root)? {
                let unescaped_path = escaped_path.replace("%2F", "/");
                if let Ok(repository) = format!("{host}/{unescaped_path}").parse::<Repository>() {
                    repositories.push(repository);
                }
            }
        }

        Ok(repositories)
    }
}

impl RepositoryStore for FileRepositoryStore {
    fn list(&self) -> Result<Vec<Repository>, FileSystemError> {
        let mut repositories = self.list_local()?;
        repositories.extend(self.list_upstream()?);
        Ok(repositories)
    }

    fn delete(&self, repository: &Repository) -> Result<(), FileSystemError> {
        let mut path = self.repository_root.clone();
        path.push(repository.storage_path());
        self.folder_deleter.delete(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{Entry, MockFolder, MockFolderDeleter};

    #[test]
    fn test_delete_should_remove_the_repositorys_own_directory() {
        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter
            .expect_delete()
            .withf(|path| path.to_str() == Some("/repositories/local/hello-world"))
            .returning(|_| Ok(()));

        let store = FileRepositoryStore::new(
            PathBuf::from("/repositories"),
            Arc::new(MockFolder::new()),
            Arc::new(folder_deleter),
        );

        let repository: Repository = "hello-world".parse().unwrap();
        let result = store.delete(&repository);

        assert!(result.is_ok());
    }

    #[test]
    fn test_delete_should_remove_an_upstream_repositorys_own_directory() {
        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter
            .expect_delete()
            .withf(|path| path.to_str() == Some("/repositories/upstream/ghcr.io/foo%2Fbar"))
            .returning(|_| Ok(()));

        let store = FileRepositoryStore::new(
            PathBuf::from("/repositories"),
            Arc::new(MockFolder::new()),
            Arc::new(folder_deleter),
        );

        let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();
        let result = store.delete(&repository);

        assert!(result.is_ok());
    }

    mod list {
        use super::*;

        #[test]
        fn test_should_return_nothing_when_neither_subtree_exists() {
            let mut folder = MockFolder::new();
            folder.expect_exists().returning(|_| false);

            let store = FileRepositoryStore::new(
                PathBuf::from("/repositories"),
                Arc::new(folder),
                Arc::new(MockFolderDeleter::new()),
            );

            assert_eq!(store.list().unwrap(), Vec::new());
        }

        #[test]
        fn test_should_list_local_repositories() {
            let mut folder = MockFolder::new();
            folder
                .expect_exists()
                .returning(|path| path == std::path::Path::new("/repositories/local"));
            folder.expect_entries().returning(|_| {
                Ok(vec![
                    Entry::create_directory("hello-world"),
                    Entry::create_file_entry("not-a-repository"),
                ])
            });

            let store = FileRepositoryStore::new(
                PathBuf::from("/repositories"),
                Arc::new(folder),
                Arc::new(MockFolderDeleter::new()),
            );

            assert_eq!(
                store.list().unwrap(),
                vec!["hello-world".parse::<Repository>().unwrap()]
            );
        }

        #[test]
        fn test_should_list_upstream_repositories_unescaping_nested_paths() {
            let mut folder = MockFolder::new();
            folder.expect_exists().returning(|_| true);
            folder.expect_entries().returning(|path| {
                if path == std::path::Path::new("/repositories/local") {
                    Ok(Vec::new())
                } else if path == std::path::Path::new("/repositories/upstream") {
                    Ok(vec![Entry::create_directory("ghcr.io")])
                } else if path == std::path::Path::new("/repositories/upstream/ghcr.io") {
                    Ok(vec![Entry::create_directory("foo%2Fbar")])
                } else {
                    Ok(Vec::new())
                }
            });

            let store = FileRepositoryStore::new(
                PathBuf::from("/repositories"),
                Arc::new(folder),
                Arc::new(MockFolderDeleter::new()),
            );

            assert_eq!(
                store.list().unwrap(),
                vec!["ghcr.io/foo/bar".parse::<Repository>().unwrap()]
            );
        }
    }
}
