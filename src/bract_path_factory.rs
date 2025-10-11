use std::{path::PathBuf, sync::Arc};

use file_system::{FileSystemError, Folder};

pub struct BractPathFactory {
    folder: Arc<dyn Folder + Send + Sync>,
}

impl BractPathFactory {
    pub fn new(folder: Arc<dyn Folder + Send + Sync>) -> Self {
        Self { folder }
    }

    pub fn bract_socket_path(&self) -> Result<PathBuf, FileSystemError> {
        let mut path = self.folder.executable_root()?;
        path.push("bract.socket");
        Ok(path)
    }

    pub fn token_path(&self) -> Result<PathBuf, FileSystemError> {
        let mut path = self.folder.executable_root()?;
        path.push("bract.token");
        Ok(path)
    }
}
