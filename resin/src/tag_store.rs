use crate::{
    digest::{Digest, DigestError},
    name::Name,
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

#[cfg_attr(test, mockall::automock)]
pub trait TagStore: Send + Sync {
    fn list(&self, name: &Name) -> Result<Vec<String>, TagStoreError>;
    fn read(&self, name: &Name, tag: &str) -> Result<Digest, TagStoreError>;
    fn write(&self, name: &Name, tag: &str, digest: &Digest) -> Result<(), TagStoreError>;
    fn delete(&self, name: &Name, tag: &str) -> Result<bool, TagStoreError>;
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

    fn get_repository_path(&self, name: &Name) -> PathBuf {
        let mut result = self.root.clone();
        result.push(name.fs_safe());
        result
    }

    fn get_tags_path(&self, name: &Name) -> PathBuf {
        let mut result = self.get_repository_path(name);
        result.push("_manifests");
        result.push("tags");
        result
    }

    fn assert_repository_exists(&self, name: &Name) -> Result<(), TagStoreError> {
        let repository_path = self.get_repository_path(name);
        if self.folder.exists(&repository_path) {
            Ok(())
        } else {
            Err(UnknownRepository(name.to_string()))
        }
    }
}

impl TagStore for FileTagStore {
    fn list(&self, name: &Name) -> Result<Vec<String>, TagStoreError> {
        self.assert_repository_exists(name)?;

        let tag_path = self.get_tags_path(name);
        if !self.folder.exists(&tag_path) {
            return Ok(Vec::new());
        }

        let mut tags: Vec<String> = self
            .folder
            .entries(&tag_path)?
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .map(|file| file.name.clone())
            .collect();
        tags.sort();

        Ok(tags)
    }

    fn read(&self, name: &Name, tag: &str) -> Result<Digest, TagStoreError> {
        self.assert_repository_exists(name)?;

        let mut tag_path = self.get_tags_path(name);
        tag_path.push(tag);

        if self.file_reader.exists(&tag_path) {
            let contents = self.file_reader.read_all(&tag_path)?;
            let digest: Digest = contents.trim().parse()?;
            Ok(digest)
        } else {
            Err(TagStoreError::UnknwonTag {
                repository: name.to_string(),
                tag: tag.to_string(),
            })
        }
    }

    fn write(&self, name: &Name, tag: &str, digest: &Digest) -> Result<(), TagStoreError> {
        self.assert_repository_exists(name)?;

        let mut tags_path = self.get_tags_path(name);
        if !self.folder.exists(&tags_path) {
            self.folder.create_recursively(&tags_path)?;
        }

        tags_path.push(tag);
        self.file_writer
            .write_all(&tags_path, &digest.to_string())?;

        Ok(())
    }

    fn delete(&self, name: &Name, tag: &str) -> Result<bool, TagStoreError> {
        self.assert_repository_exists(name)?;

        let mut tag_path = self.get_tags_path(name);
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
        use crate::{
            name::Name,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{Entry, MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, str::FromStr, sync::Arc};

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
                store.list(&Name::from_str("oops").expect("parsable")),
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
                store.list(&Name::from_str("foo").expect("parsable")),
                Ok(tags) if tags.is_empty()
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
                    Entry::create_directory("foo"),
                    Entry::create_file_entry("bar"),
                ],
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.list(&Name::from_str("oops").expect("parsable")),
                Ok(items) if items == vec!["bar".to_string()]
            ));
        }

        #[test]
        fn should_return_tags_sorted_lexicographically() {
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
                    Entry::create_file_entry("v2"),
                    Entry::create_file_entry("latest"),
                    Entry::create_file_entry("v1"),
                ],
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.list(&Name::from_str("oops").expect("parsable")),
                Ok(items) if items == vec!["latest".to_string(), "v1".to_string(), "v2".to_string()]
            ));
        }

        #[test]
        fn should_return_error_if_namespaced_repository_does_not_exist() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_does_not_exist("/tmp/myns%2Foops/");

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.list(&Name::from_namespaced("myns", "oops").expect("parsable")),
                Err(TagStoreError::UnknownRepository(r)) if r == "myns/oops"
            ))
        }
    }

    mod read {
        use crate::{
            digest::DigestError,
            name::Name,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, str::FromStr, sync::Arc};

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
                store.read(&Name::from_str("oops").expect("parsable"), "foo"),
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
                store.read(&Name::from_str("foo").expect("parsable"), "bar"),
                Err(TagStoreError::UnknwonTag { repository: r, tag: t }) if r == "foo" && t == "bar"
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
                store.read(&Name::from_str("foo").expect("parsable"), "bar"),
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

            assert!(matches!(
                store.read(&Name::from_str("foo").expect("parsable"), "bar"),
                Ok(digest) if digest.hex() == sha
            ));
        }

        #[test]
        fn should_return_digest_when_file_has_trailing_newline() {
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
                &format!("sha256:{sha}\n"),
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.read(&Name::from_str("foo").expect("parsable"), "bar"),
                Ok(digest) if digest.hex() == sha
            ));
        }

        #[test]
        fn should_use_escaped_path_for_namespaced_name() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let mut file_reader = MockFileReader::new();
            let file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            folder.given_exists("/tmp/myns%2Ffoo/");
            file_reader.given_exists("/tmp/myns%2Ffoo/_manifests/tags/bar");
            let sha = "ff".repeat(32);
            file_reader.given_can_read_all_with_contents(
                "/tmp/myns%2Ffoo/_manifests/tags/bar",
                &format!("sha256:{sha}"),
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.read(&Name::from_namespaced("myns", "foo").expect("parsable"), "bar"),
                Ok(digest) if digest.hex() == sha
            ));
        }
    }

    mod write {
        use crate::{
            digest::Digest,
            name::Name,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, str::FromStr, sync::Arc};

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
                store.write(&Name::from_str("oops").expect("parsable"), "foo", &digest),
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

            assert!(matches!(
                store.write(&Name::from_str("foo").expect("parsable"), "bar", &digest),
                Ok(())
            ))
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

            assert!(matches!(
                store.write(&Name::from_str("foo").expect("parsable"), "bar", &digest),
                Ok(())
            ))
        }

        #[test]
        fn should_write_tags_using_escaped_path_for_namespaced_name() {
            let root = PathBuf::from("/tmp");
            let mut folder = MockFolder::new();
            let file_reader = MockFileReader::new();
            let mut file_writer = MockFileWriter::new();
            let file_deleter = MockFileDeleter::new();

            let sha = "ff".repeat(32);
            let digest = Digest(format!("sha256:{sha}"));

            folder.given_exists("/tmp/myns%2Ffoo/");
            folder.given_exists("/tmp/myns%2Ffoo/_manifests/tags");
            file_writer.expect_write_to_file_with_contents(
                "/tmp/myns%2Ffoo/_manifests/tags/bar",
                &digest.to_string(),
            );

            let store = FileTagStore::new(
                root,
                Arc::new(folder),
                Arc::new(file_reader),
                Arc::new(file_writer),
                Arc::new(file_deleter),
            );

            assert!(matches!(
                store.write(
                    &Name::from_namespaced("myns", "foo").expect("parsable"),
                    "bar",
                    &digest
                ),
                Ok(())
            ))
        }
    }

    mod delete {
        use crate::{
            name::Name,
            tag_store::{FileTagStore, TagStore, TagStoreError},
        };
        use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockFolder};
        use std::{path::PathBuf, str::FromStr, sync::Arc};

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
                store.delete(&Name::from_str("oops").expect("parsable"), "foo"),
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

            assert!(matches!(
                store.delete(&Name::from_str("foo").expect("parsable"), "bar"),
                Ok(false)
            ))
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

            assert!(matches!(
                store.delete(&Name::from_str("foo").expect("parsable"), "bar"),
                Ok(true)
            ))
        }
    }
}
