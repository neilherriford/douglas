use crate::{
    digest::{Digest, DigestError},
    tag_store::TagStoreError::UnknownRepository,
};
use file_system::{EntryKind, FileDeleter, FileReader, FileSystemError, FileWriter, Folder};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TagStoreError {
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),

    #[error("Digest Error {0}")]
    DigestError(#[from] DigestError),

    #[error("Unknown repository: {0}")]
    UnknownRepository(String),

    #[error("Unknown tag: {repository}::{tag}")]
    UnknwonTag { repository: String, tag: String },
}

trait TagStore {
    fn list(&self, repository: &str) -> Result<Vec<String>, TagStoreError>;
    fn read(&self, repository: &str, tag: &str) -> Result<Digest, TagStoreError>;
    fn write(&self, repository: &str, tag: &str, digest: &Digest) -> Result<(), TagStoreError>;
    fn delete(&self, repository: &str, tag: &str) -> Result<bool, TagStoreError>;
}

pub struct FileTagStore {
    root: PathBuf,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    file_deleter: Arc<dyn FileDeleter>,
}

impl FileTagStore {
    pub fn new(
        root: PathBuf,
        folder: Arc<dyn Folder>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Self {
        Self {
            root,
            folder,
            file_reader,
            file_writer,
            file_deleter,
        }
    }

    fn get_repository_path(&self, repository: &str) -> PathBuf {
        let mut result = self.root.clone();
        result.push(repository);
        result
    }

    fn get_tags_path(&self, repository: &str) -> PathBuf {
        let mut result = self.get_repository_path(repository);
        result.push("_manifests");
        result.push("tags");
        result
    }

    fn assert_repository_exists(&self, repository: &str) -> Result<(), TagStoreError> {
        let repository_path = self.get_repository_path(repository);
        if self.folder.exists(&repository_path) {
            Ok(())
        } else {
            Err(UnknownRepository(repository.to_string()))
        }
    }
}

impl TagStore for FileTagStore {
    fn list(&self, repository: &str) -> Result<Vec<String>, TagStoreError> {
        self.assert_repository_exists(repository)?;

        let tag_path = self.get_tags_path(repository);
        if !self.folder.exists(&tag_path) {
            return Ok(Vec::new());
        }

        Ok(self
            .folder
            .entries(&tag_path)?
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|file| file.name.clone())
            .collect())
    }

    fn read(&self, repository: &str, tag: &str) -> Result<Digest, TagStoreError> {
        self.assert_repository_exists(repository)?;

        let mut tag_path = self.get_tags_path(repository);
        tag_path.push(tag);

        if self.file_reader.exists(&tag_path) {
            let contents = self.file_reader.read_all(&tag_path)?;
            let digest: Digest = contents.parse()?;
            Ok(digest)
        } else {
            Err(TagStoreError::UnknwonTag {
                repository: repository.to_string(),
                tag: tag.to_string(),
            })
        }
    }

    fn write(&self, repository: &str, tag: &str, digest: &Digest) -> Result<(), TagStoreError> {
        self.assert_repository_exists(repository)?;

        let mut tags_path = self.get_tags_path(repository);
        if !self.folder.exists(&tags_path) {
            self.folder.create_recursively(&tags_path)?;
        }

        tags_path.push(tag);
        self.file_writer
            .write_all(&tags_path, &digest.to_string())?;

        Ok(())
    }

    fn delete(&self, repository: &str, tag: &str) -> Result<bool, TagStoreError> {
        self.assert_repository_exists(repository)?;

        let mut tag_path = self.get_tags_path(repository);
        tag_path.push(tag);

        if self.file_reader.exists(&tag_path) {
            self.file_deleter.delete(&tag_path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    mod list {
        use crate::tag_store::{FileTagStore, TagStore, TagStoreError};
        use file_system::{
            Entry, EntryKind, MockFileDeleter, MockFileReader, MockFileWriter, MockFolder,
        };
        use std::{path::PathBuf, sync::Arc};

        #[test]
        fn should_return_error_if_repository_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_does_not_exist("/tmp/oops/");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                    store.list("oops"),
                    Err(TagStoreError::UnknownRepository(r)) if r == "oops"
            ))
        }

        #[test]
        fn should_return_empty_if_tag_directory_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            folder.given_does_not_exist("/tmp/foo/_manifests/tags");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                    store.list("foo"),
                    Ok(t) if t.is_empty()
            ))
        }

        #[test]
        fn should_return_files_only() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/oops/");
            folder.given_exists("/tmp/oops/_manifests/tags");
            folder.given_folder_entries(
                "/tmp/oops/_manifests/tags",
                vec![
                    Entry {
                        name: "foo".to_string(),
                        kind: EntryKind::Directory,
                        is_link: false,
                    },
                    Entry {
                        name: "bar".to_string(),
                        kind: EntryKind::File,
                        is_link: false,
                    },
                ],
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(store.list("oops"), Ok(items) if items == vec!["bar".to_string()]));
        }
    }

    mod read {
        use crate::{
            digest::DigestError,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, sync::Arc};

        #[test]
        fn should_return_error_if_repository_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_does_not_exist("/tmp/oops/");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.read("oops", "foo"),
                Err(TagStoreError::UnknownRepository(r)) if r == "oops"
            ))
        }

        #[test]
        fn should_return_error_if_tag_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            file_reader.given_does_not_exist("/tmp/foo/_manifests/tags/bar");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.read("foo", "bar"),
                Err(TagStoreError::UnknwonTag {
                    repository: r,
                    tag: t
                }) if r == "foo" && t == "bar"
            ));
        }

        #[test]
        fn should_return_error_if_tag_does_not_have_valid_sha() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            file_reader.given_exists("/tmp/foo/_manifests/tags/bar");
            file_reader.given_can_read_all_with_contents("/tmp/foo/_manifests/tags/bar", "whoops");
            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.read("foo", "bar"),
                Err(TagStoreError::DigestError(DigestError::InvalidDigest))
            ));
        }

        #[test]
        fn should_return_digest() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            file_reader.given_exists("/tmp/foo/_manifests/tags/bar");
            let sha = "ff".repeat(32);
            file_reader.given_can_read_all_with_contents(
                "/tmp/foo/_manifests/tags/bar",
                &format!("sha256:{sha}"),
            );
            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            let actual = store.read("foo", "bar");
            assert!(matches!(
                actual,
                Ok(d) if d.hex() == sha
            ));
        }
    }

    mod write {
        use crate::{
            digest::Digest,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, sync::Arc};

        #[test]
        fn should_return_error_if_repository_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_does_not_exist("/tmp/oops/");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            let sha = "ff".repeat(32);
            let digest = Digest(format!("sha256:{sha}"));

            assert!(matches!(
                store.write("oops", "foo", &digest),
                Err(TagStoreError::UnknownRepository(r)) if r == "oops"
            ))
        }

        #[test]
        fn should_return_write_tags() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            let sha = "ff".repeat(32);
            let digest = Digest(format!("sha256:{sha}"));

            folder.given_exists("/tmp/foo/");
            folder.given_exists("/tmp/foo/_manifests/tags");
            file_writer.expect_write_to_file_with_contents(
                "/tmp/foo/_manifests/tags/bar",
                &digest.to_string(),
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(store.write("foo", "bar", &digest), Ok(())))
        }

        #[test]
        fn should_return_write_tags_creating_support_directories() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            let sha = "ff".repeat(32);
            let digest = Digest(format!("sha256:{sha}"));

            folder.given_exists("/tmp/foo/");
            folder.given_does_not_exist("/tmp/foo/_manifests/tags");
            folder.expect_create_folder_recursively_with("/tmp/foo/_manifests/tags");
            file_writer.expect_write_to_file_with_contents(
                "/tmp/foo/_manifests/tags/bar",
                &digest.to_string(),
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(store.write("foo", "bar", &digest), Ok(())))
        }
    }

    mod delete {
        use crate::tag_store::{FileTagStore, TagStore, TagStoreError};
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, sync::Arc};

        #[test]
        fn should_return_error_if_repository_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_does_not_exist("/tmp/oops/");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.delete("oops", "foo"),
                Err(TagStoreError::UnknownRepository(r)) if r == "oops"
            ))
        }

        #[test]
        fn should_return_false_if_no_such_tag() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            file_reader.given_does_not_exist("/tmp/foo/_manifests/tags/bar");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(store.delete("foo", "bar"), Ok(false)))
        }

        #[test]
        fn should_return_true_if_tag_exists() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let mut file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/foo/");
            file_reader.given_exists("/tmp/foo/_manifests/tags/bar");
            file_deleter.expect_file_to_be_deleted("/tmp/foo/_manifests/tags/bar");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(store.delete("foo", "bar"), Ok(true)))
        }
    }
}
