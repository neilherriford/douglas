use crate::digest::Digest;
use file_system::{FileDeleter, FileRenamer, FileSystemError, FileWriter};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub fn create_sharded_blob_root(root: &Path, digest: &Digest) -> PathBuf {
    let mut result = root.to_path_buf();
    result.push("blobs");
    result.push("sha256");
    let hex = digest.hex();
    let prefix = &hex[0..2];

    result.push(prefix);
    result.push(hex);

    result
}

pub struct BlobFilePaths {
    pub final_root: PathBuf,
    pub final_file: PathBuf,
    pub mediatype_file: PathBuf,
}

impl BlobFilePaths {
    pub fn new(root: &Path, digest: &Digest) -> Self {
        let final_root = create_sharded_blob_root(root, digest);

        let mut final_file = final_root.clone();
        final_file.push(digest.hex());

        let mut mediatype_file = final_root.clone();
        mediatype_file.push(format!("{}.mediatype", digest.hex()));

        Self {
            final_root,
            final_file,
            mediatype_file,
        }
    }
}

pub struct BlobCommit {
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    file_writer: Arc<dyn FileWriter>,
    completed: bool,
    temp_file: PathBuf,
    final_file: PathBuf,
    mediatype_file: PathBuf,
}

impl BlobCommit {
    pub fn new(
        temp_file: &Path,
        paths: &BlobFilePaths,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
        file_writer: Arc<dyn FileWriter>,
    ) -> Self {
        Self {
            file_renamer,
            file_deleter,
            file_writer,
            completed: false,
            temp_file: temp_file.to_path_buf(),
            final_file: paths.final_file.clone(),
            mediatype_file: paths.mediatype_file.clone(),
        }
    }

    pub fn temp_file(&self) -> &Path {
        &self.temp_file
    }

    pub fn complete(&mut self, mediatype: &str) -> Result<(), FileSystemError> {
        self.file_renamer
            .rename(&self.temp_file, &self.final_file)?;

        match self.file_writer.write_all(&self.mediatype_file, mediatype) {
            Ok(()) => {
                self.completed = true;
                Ok(())
            }
            Err(err) => {
                self.clean_up()?;
                Err(err)
            }
        }
    }

    pub fn clean_up(&mut self) -> Result<(), FileSystemError> {
        self.file_deleter.delete(&self.temp_file)?;
        self.file_deleter.delete(&self.final_file)?;
        self.file_deleter.delete(&self.mediatype_file)?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for BlobCommit {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        let _ = self.clean_up();
    }
}
