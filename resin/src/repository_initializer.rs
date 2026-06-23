use std::{path::PathBuf, sync::Arc};

use file_system::{FileSystemError, Folder};

pub trait RepositoryInitializer: Send + Sync {
    fn initalize(&self, name: &str) -> Result<(), FileSystemError>;
}

#[derive(Clone)]
pub struct FileRepositoryInitializer {
    root: PathBuf,
    folder: Arc<dyn Folder>,
}

impl FileRepositoryInitializer {
    pub fn new(root: PathBuf, folder: Arc<dyn Folder>) -> Self {
        Self { root, folder }
    }

    fn create_support_paths(&self, name: &str) -> (PathBuf, PathBuf) {
        let mut buffer = self.root.clone();
        buffer.push("repositories");
        buffer.push(name);
        buffer.push("_manifests");
        let mut revisions = buffer.clone();
        revisions.push("revisions");
        revisions.push("sha256");

        let mut tags = buffer.clone();
        tags.push("tags");

        (revisions, tags)
    }
}

impl RepositoryInitializer for FileRepositoryInitializer {
    fn initalize(&self, name: &str) -> Result<(), FileSystemError> {
        let (revisions, tags) = self.create_support_paths(name);

        self.folder.create_recursively(&revisions)?;
        self.folder.create_recursively(&tags)?;

        Ok(())
    }
}
