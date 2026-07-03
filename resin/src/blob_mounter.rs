use crate::{
    blob_paths::BlobFilePaths, digest::Digest, name::Name, repository_store::RepositoryStore,
};
use file_system::{FileDeleter, FileSystemError, Folder, Links};
use std::{path::PathBuf, sync::Arc};

pub trait BlobMounter: Send + Sync {
    fn mount_blob(
        &self,
        source_registry: Option<&Name>,
        digest: &Digest,
        destination_registry: &Name,
    ) -> Result<bool, FileSystemError>;
}

pub struct FileBlobMounter {
    repository_store: Arc<dyn RepositoryStore>,
    folder: Arc<dyn Folder>,
    file_deleter: Arc<dyn FileDeleter>,
    links: Arc<dyn Links>,
    repositories_root: PathBuf,
}

impl FileBlobMounter {
    pub fn new(
        repository_store: Arc<dyn RepositoryStore>,
        folder: Arc<dyn Folder>,
        file_deleter: Arc<dyn FileDeleter>,
        links: Arc<dyn Links>,
        repositories_root: PathBuf,
    ) -> Self {
        Self {
            repository_store,
            folder,
            file_deleter,
            links,
            repositories_root,
        }
    }

    fn repository_root(&self, name: &Name) -> PathBuf {
        let mut result = self.repositories_root.clone();
        result.push(name.fs_safe());
        result
    }

    fn search_in_repository(
        &self,
        repository: &Name,
        digest: &Digest,
    ) -> Result<Option<BlobFilePaths>, FileSystemError> {
        let root = self.repository_root(repository);
        if !self.folder.exists(&root) {
            return Ok(None);
        }

        let paths = BlobFilePaths::new(&root, digest);
        if self.folder.exists(&paths.final_file) && self.folder.exists(&paths.mediatype_file) {
            Ok(Some(paths))
        } else {
            Ok(None)
        }
    }

    fn search_across_all_repositories(
        &self,
        digest: &Digest,
        skip: Option<&Name>,
    ) -> Result<Option<BlobFilePaths>, FileSystemError> {
        for repository in self.repository_store.list()? {
            if Some(&repository) == skip {
                continue;
            }
            if let Some(found) = self.search_in_repository(&repository, digest)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
}

impl BlobMounter for FileBlobMounter {
    fn mount_blob(
        &self,
        source_registry: Option<&Name>,
        digest: &Digest,
        destination_registry: &Name,
    ) -> Result<bool, FileSystemError> {
        let found = match source_registry {
            Some(source_registry) => match self.search_in_repository(source_registry, digest)? {
                Some(found) => Some(found),
                None => self.search_across_all_repositories(digest, Some(source_registry))?,
            },
            None => self.search_across_all_repositories(digest, None)?,
        };

        let Some(source_paths) = found else {
            return Ok(false);
        };

        let destination_root = self.repository_root(destination_registry);
        let destination_paths = BlobFilePaths::new(&destination_root, digest);
        self.folder
            .create_recursively(&destination_paths.final_root)?;

        self.links
            .create_hard(&source_paths.final_file, &destination_paths.final_file)?;

        if let Err(err) = self.links.create_hard(
            &source_paths.mediatype_file,
            &destination_paths.mediatype_file,
        ) {
            let _ = self.file_deleter.delete(&destination_paths.final_file);
            return Err(err);
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    mod mount_blob {
        use crate::{
            blob_mounter::{BlobMounter, FileBlobMounter},
            digest::Digest,
            name::Name,
            repository_store::MockRepositoryStore,
        };
        use file_system::{FileDeleter, Folder, Links, MockFileDeleter, MockFolder, MockLinks};
        use std::{path::PathBuf, str::FromStr, sync::Arc};

        const HEX: &str = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
        const PREFIX: &str = "1b";

        fn digest() -> Digest {
            Digest(format!("sha256:{HEX}"))
        }

        fn blob_paths(registry: &str) -> (String, String) {
            let final_file = format!("/tmp/{registry}/blobs/sha256/{PREFIX}/{HEX}/{HEX}");
            let mediatype_file = format!("{final_file}.mediatype");
            (final_file, mediatype_file)
        }

        #[test]
        fn test_should_mount_directly_when_hint_has_the_blob() {
            let mut folder = MockFolder::new();
            let file_deleter = MockFileDeleter::new();
            let mut links = MockLinks::new();
            let repository_store = MockRepositoryStore::new();

            let source = Name::from_str("source").unwrap();
            let destination = Name::from_str("dest").unwrap();
            let (source_final, source_mediatype) = blob_paths("source");
            let (dest_final, dest_mediatype) = blob_paths("dest");

            folder
                .given_exists("/tmp/source")
                .given_exists(&source_final)
                .given_exists(&source_mediatype)
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/dest/blobs/sha256/{PREFIX}/{HEX}"
                ));

            links
                .expect_create_hard_with(&source_final, &dest_final)
                .expect_create_hard_with(&source_mediatype, &dest_mediatype);

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(Some(&source), &digest(), &destination);
            assert!(matches!(actual, Ok(true)));
        }

        #[test]
        fn test_should_fall_back_to_scan_when_hint_misses() {
            let mut folder = MockFolder::new();
            let file_deleter = MockFileDeleter::new();
            let mut links = MockLinks::new();
            let mut repository_store = MockRepositoryStore::new();

            let source = Name::from_str("source").unwrap();
            let other = Name::from_str("other").unwrap();
            let destination = Name::from_str("dest").unwrap();
            let (other_final, other_mediatype) = blob_paths("other");
            let (dest_final, dest_mediatype) = blob_paths("dest");

            folder
                .given_exists("/tmp/source")
                .given_does_not_exist(&blob_paths("source").0)
                .given_exists("/tmp/other")
                .given_exists(&other_final)
                .given_exists(&other_mediatype)
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/dest/blobs/sha256/{PREFIX}/{HEX}"
                ));

            repository_store
                .expect_list()
                .returning(move || Ok(vec![source.clone(), other.clone()]));

            links
                .expect_create_hard_with(&other_final, &dest_final)
                .expect_create_hard_with(&other_mediatype, &dest_mediatype);

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(
                Some(&Name::from_str("source").unwrap()),
                &digest(),
                &destination,
            );
            assert!(matches!(actual, Ok(true)));
        }

        #[test]
        fn test_should_skip_repository_search_when_hint_root_does_not_exist() {
            let mut folder = MockFolder::new();
            let file_deleter = MockFileDeleter::new();
            let mut links = MockLinks::new();
            let mut repository_store = MockRepositoryStore::new();

            let missing = Name::from_str("missing").unwrap();
            let other = Name::from_str("other").unwrap();
            let destination = Name::from_str("dest").unwrap();
            let (other_final, other_mediatype) = blob_paths("other");
            let (dest_final, dest_mediatype) = blob_paths("dest");

            folder
                .given_does_not_exist("/tmp/missing")
                .given_exists("/tmp/other")
                .given_exists(&other_final)
                .given_exists(&other_mediatype)
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/dest/blobs/sha256/{PREFIX}/{HEX}"
                ));

            repository_store
                .expect_list()
                .returning(move || Ok(vec![missing.clone(), other.clone()]));

            links
                .expect_create_hard_with(&other_final, &dest_final)
                .expect_create_hard_with(&other_mediatype, &dest_mediatype);

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(
                Some(&Name::from_str("missing").unwrap()),
                &digest(),
                &destination,
            );
            assert!(matches!(actual, Ok(true)));
        }

        #[test]
        fn test_should_scan_all_repositories_when_no_hint_given() {
            let mut folder = MockFolder::new();
            let file_deleter = MockFileDeleter::new();
            let mut links = MockLinks::new();
            let mut repository_store = MockRepositoryStore::new();

            let foo = Name::from_str("foo").unwrap();
            let bar = Name::from_str("bar").unwrap();
            let destination = Name::from_str("dest").unwrap();
            let (foo_final, _) = blob_paths("foo");
            let (bar_final, bar_mediatype) = blob_paths("bar");
            let (dest_final, dest_mediatype) = blob_paths("dest");

            folder
                .given_exists("/tmp/foo")
                .given_does_not_exist(&foo_final)
                .given_exists("/tmp/bar")
                .given_exists(&bar_final)
                .given_exists(&bar_mediatype)
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/dest/blobs/sha256/{PREFIX}/{HEX}"
                ));

            repository_store
                .expect_list()
                .returning(move || Ok(vec![foo.clone(), bar.clone()]));

            links
                .expect_create_hard_with(&bar_final, &dest_final)
                .expect_create_hard_with(&bar_mediatype, &dest_mediatype);

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(None, &digest(), &destination);
            assert!(matches!(actual, Ok(true)));
        }

        #[test]
        fn test_should_return_false_when_not_found_anywhere() {
            let mut folder = MockFolder::new();
            let file_deleter = MockFileDeleter::new();
            let links = MockLinks::new();
            let mut repository_store = MockRepositoryStore::new();

            let foo = Name::from_str("foo").unwrap();
            let bar = Name::from_str("bar").unwrap();
            let destination = Name::from_str("dest").unwrap();

            folder
                .given_does_not_exist("/tmp/foo")
                .given_does_not_exist("/tmp/bar");

            repository_store
                .expect_list()
                .returning(move || Ok(vec![foo.clone(), bar.clone()]));

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(None, &digest(), &destination);
            assert!(matches!(actual, Ok(false)));
        }

        #[test]
        fn test_should_clean_up_content_link_when_mediatype_link_fails() {
            let mut folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut links = MockLinks::new();
            let repository_store = MockRepositoryStore::new();

            let source = Name::from_str("source").unwrap();
            let destination = Name::from_str("dest").unwrap();
            let (source_final, source_mediatype) = blob_paths("source");
            let (dest_final, dest_mediatype) = blob_paths("dest");

            folder
                .given_exists("/tmp/source")
                .given_exists(&source_final)
                .given_exists(&source_mediatype)
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/dest/blobs/sha256/{PREFIX}/{HEX}"
                ));

            links
                .expect_create_hard_with(&source_final, &dest_final)
                .given_create_hard_fails_once_with(
                    &source_mediatype,
                    &dest_mediatype,
                    file_system::FileSystemError::ExpectedFileError,
                );

            file_deleter.expect_file_to_be_deleted(&dest_final);

            let mounter = FileBlobMounter::new(
                Arc::new(repository_store),
                Arc::new(folder) as Arc<dyn Folder>,
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                Arc::new(links) as Arc<dyn Links>,
                PathBuf::from("/tmp"),
            );

            let actual = mounter.mount_blob(Some(&source), &digest(), &destination);
            assert!(matches!(
                actual,
                Err(file_system::FileSystemError::ExpectedFileError)
            ));
        }
    }
}
