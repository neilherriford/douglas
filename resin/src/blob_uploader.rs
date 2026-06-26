use crate::{
    blob_paths::BlobPaths,
    digest::{Digest, DigestError},
};
use file_system::{
    EntryKind, FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
};
use sha2::Sha256;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum BlobUploaderError {
    #[error("Invalid repository: '{0}'")]
    InvalidRespository(String),
    #[error("Unknown uuid: '{0}'")]
    UnknownUuid(Uuid),
    #[error("Digest mismatch: claimed {claimed}, computed {computed}")]
    DigestMismatch { claimed: Digest, computed: Digest },
    #[error("File system error")]
    FileSystemError(#[from] FileSystemError),
    #[error("Digest error ")]
    DigestError(#[from] DigestError),
}

struct PartialUpload {
    path: PathBuf,
    hasher: Option<Sha256>,
    offset: u64,
    file_deleter: Arc<dyn FileDeleter>,
    complete: bool,
}

impl PartialUpload {
    pub fn new(path: PathBuf, file_deleter: Arc<dyn FileDeleter>) -> Self {
        Self {
            path: path.clone(),
            hasher: Some(<Sha256 as sha2::Digest>::new()),
            offset: 0,
            file_deleter,
            complete: false,
        }
    }

    pub fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for PartialUpload {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.file_deleter.delete(&self.path);
            self.complete = true
        }
    }
}

pub trait BlobUploader {
    fn start(&self, registry: &str) -> Result<Uuid, BlobUploaderError>;
    fn write_chunk(&self, uuid: Uuid, chunk: &[u8]) -> Result<u64, BlobUploaderError>;
    fn complete(
        &self,
        uuid: Uuid,
        digest: &Digest,
        media_type: &str,
    ) -> Result<(), BlobUploaderError>;
    fn abort(&self, uuid: Uuid) -> Result<(), BlobUploaderError>;
    fn purge(&self) -> Result<(), BlobUploaderError>;
}

pub struct FileBlobUploader {
    temp_root: PathBuf,
    blob_root: PathBuf,
    state: Arc<Mutex<HashMap<Uuid, PartialUpload>>>,
    folder: Arc<dyn Folder>,
    file_writer: Arc<dyn FileWriter>,
    file_appender: Arc<dyn FileAppender>,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    uuid_factory: Arc<dyn Fn() -> Uuid + Send + Sync>,
}

impl FileBlobUploader {
    pub fn new(
        temp_root: PathBuf,
        blob_root: PathBuf,
        folder: Arc<dyn Folder>,
        file_writer: Arc<dyn FileWriter>,
        file_appender: Arc<dyn FileAppender>,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Self {
        let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));

        Self {
            temp_root,
            blob_root,
            state,
            folder,
            file_writer,
            file_appender,
            file_renamer,
            file_deleter,
            uuid_factory: Arc::new(Uuid::now_v7),
        }
    }
}

impl BlobUploader for FileBlobUploader {
    fn start(&self, _registry: &str) -> Result<Uuid, BlobUploaderError> {
        let uuid = (self.uuid_factory)();
        let mut temp_file_path = self.temp_root.clone();
        self.folder.create_recursively(&temp_file_path)?;
        temp_file_path.push(uuid.to_string());

        self.state.lock().unwrap().insert(
            uuid,
            PartialUpload::new(temp_file_path, Arc::clone(&self.file_deleter)),
        );

        Ok(uuid)
    }

    fn write_chunk(&self, uuid: Uuid, chunk: &[u8]) -> Result<u64, BlobUploaderError> {
        let mut state = self.state.lock().unwrap();

        let Some(upload) = state.get_mut(&uuid) else {
            return Err(BlobUploaderError::UnknownUuid(uuid));
        };

        self.file_appender.append_all_bytes(&upload.path, chunk)?;
        if let Some(hasher) = &mut upload.hasher {
            sha2::digest::Update::update(hasher, chunk);
        }
        upload.offset += chunk.len() as u64;

        Ok(upload.offset)
    }

    fn complete(
        &self,
        uuid: Uuid,
        claimed: &Digest,
        media_type: &str,
    ) -> Result<(), BlobUploaderError> {
        let mut state = self.state.lock().unwrap();

        let Some(mut upload) = state.remove(&uuid) else {
            return Err(BlobUploaderError::UnknownUuid(uuid));
        };

        let actual = sha2::Digest::finalize(upload.hasher.take().unwrap());
        let computed = actual.as_slice();
        if claimed.as_bytes().as_slice() != computed {
            return Err(BlobUploaderError::DigestMismatch {
                claimed: claimed.clone(),
                computed: Digest::from_bytes(&computed)?,
            });
        }

        let blob_paths = BlobPaths::new(&self.blob_root, claimed);
        self.folder.create_recursively(&blob_paths.final_root)?;

        self.file_renamer
            .rename(&upload.path, &blob_paths.final_file)?;
        self.file_writer
            .write_all(&blob_paths.mediatype_file, media_type)?;

        upload.mark_complete();
        Ok(())
    }

    fn abort(&self, uuid: Uuid) -> Result<(), BlobUploaderError> {
        let mut state = self.state.lock().unwrap();

        let Some(upload) = state.remove(&uuid) else {
            return Err(BlobUploaderError::UnknownUuid(uuid));
        };
        drop(upload);
        Ok(())
    }

    fn purge(&self) -> Result<(), BlobUploaderError> {
        let state = self.state.lock().unwrap();
        for stale_entry in self.folder.entries(&self.temp_root)?.iter().filter(|e| {
            e.kind == EntryKind::File
                && Uuid::parse_str(&e.name).is_ok_and(|uuid| !state.contains_key(&uuid))
        }) {
            let mut path = self.temp_root.clone();
            path.push(&stale_entry.name);
            self.file_deleter.delete(&path)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod partial_upload {
        use crate::blob_uploader::PartialUpload;
        use file_system::{FileDeleter, MockFileDeleter};
        use std::{path::PathBuf, sync::Arc};

        #[test]
        fn test_should_delete_if_incomplete() {
            let mut file_deleter = MockFileDeleter::new();

            file_deleter.expect_file_to_be_deleted("/tmp/file");

            let upload = PartialUpload::new(
                PathBuf::from("/tmp/file"),
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
            );

            drop(upload);
        }

        #[test]
        fn test_should_not_delete_if_complete() {
            let file_deleter = MockFileDeleter::new();

            let mut upload = PartialUpload::new(
                PathBuf::from("/tmp/file"),
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
            );

            upload.mark_complete();

            drop(upload);
        }
    }

    mod start {
        use crate::blob_uploader::{
            BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use mockall::predicate;
        use std::{
            collections::HashMap,
            path::PathBuf,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[test]
        fn test_should_fail_if_tmp_folder_cannot_be_made() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            folder
                .expect_create_recursively()
                .with(predicate::eq(temp_root.clone()))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start("foo");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn test_should_create_uuid_and_cleanup() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            folder.expect_create_folder_recursively_with("/tmp/tmp");
            file_deleter.expect_file_to_be_deleted(&format!("/tmp/tmp/{}", Uuid::max()));
            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start("foo");
            assert!(matches!(
                actual,
                Ok(u) if u == Uuid::max()));
        }
    }

    mod write_chunk {
        use crate::blob_uploader::{
            BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            path::PathBuf,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[test]
        fn test_write_chunk_should_fail_for_uuids_not_started() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.write_chunk(Uuid::max(), Vec::new().as_slice());
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid(u)) if u == Uuid::max()
            ));
        }

        #[test]
        fn test_should_fail_if_cannot_append_when_write_chunk() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            folder.expect_create_folder_recursively_with("/tmp/tmp");

            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            file_appender.given_append_all_bytes_fails_once_with(
                temp_file,
                vec![0xDE, 0xAD, 0xBE, 0xEF],
                FileSystemError::ExpectedFileError,
            );
            file_deleter.expect_file_to_be_deleted(temp_file);

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start("foo");
            assert!(matches!(
                actual,
                Ok(u) if u == Uuid::max()));
            let actual =
                blob_uploader.write_chunk(Uuid::max(), vec![0xDE, 0xAD, 0xBE, 0xEF].as_slice());
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn test_should_write_chunk() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            folder.expect_create_folder_recursively_with("/tmp/tmp");
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());

            file_appender.expect_append_all_bytes_with(temp_file, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            file_deleter.expect_file_to_be_deleted(temp_file);

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start("foo");
            assert!(matches!(
                actual,
                Ok(u) if u == Uuid::max()));
            let actual =
                blob_uploader.write_chunk(Uuid::max(), vec![0xDE, 0xAD, 0xBE, 0xEF].as_slice());
            assert!(matches!(actual, Ok(4)));
        }
    }

    mod complete {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            digest::Digest,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            path::PathBuf,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[test]
        fn test_should_return_error_if_unknown_uuid() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let claimed = format!("sha256:{}", "F".repeat(64));
            let actual = blob_uploader.complete(Uuid::max(), &Digest(claimed), "mediatype");
            assert!(matches!(actual, Err(BlobUploaderError::UnknownUuid(u)) if u == Uuid::max()));
        }

        #[test]
        fn test_should_return_error_if_digest_does_not_match() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{}", "F".repeat(64)));
            let expected_computed = Digest(
                "sha256:1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf"
                    .to_string(),
            );
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");

            let actual = blob_uploader.complete(Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                    actual,
                    Err(BlobUploaderError::DigestMismatch { claimed, computed })
                    if claimed ==given_claimed && computed == expected_computed));
        }

        #[test]
        fn test_should_return_error_and_clean_up_if_rename_fails() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.given_rename_fails_once_with(
                temp_file,
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}").to_string());
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");

            let actual = blob_uploader.complete(Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn test_should_return_error_and_clean_up_if_sidecar_fails() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.given_write_to_file_fails_once_with(
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}").to_string());
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");

            let actual = blob_uploader.complete(Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn test_should_complete() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}").to_string());
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");

            let actual = blob_uploader.complete(Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn test_should_error_with_additional_writes_after_complete() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}").to_string());
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");

            blob_uploader
                .complete(Uuid::max(), &given_claimed, "mediatype")
                .expect("should complete");
            let actual = blob_uploader.write_chunk(uuid, vec![0xC0, 0xDE].as_slice());
            assert!(matches!(actual, Err(BlobUploaderError::UnknownUuid(u)) if u == Uuid::max()));
        }
    }

    mod abort {
        use crate::blob_uploader::{
            BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileWriter, Folder, MockFileAppender,
            MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            path::PathBuf,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[test]
        fn test_should_return_error_if_unknown_uuid() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.abort(Uuid::max());
            assert!(matches!(actual, Err(BlobUploaderError::UnknownUuid(u)) if u == Uuid::max()));
        }

        #[test]
        fn test_should_clean_up() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start("foo").expect("should start");
            let actual = blob_uploader.abort(uuid);
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn test_should_not_double_clean_up() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .abort(uuid)
                .expect("should work the first time");
            let actual = blob_uploader.abort(Uuid::max());
            assert!(matches!(actual, Err(BlobUploaderError::UnknownUuid(u)) if u == uuid));
        }
    }

    mod purge {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            digest::Digest,
        };
        use file_system::{
            Entry, FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            path::PathBuf,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[test]
        fn test_should_error_if_delete_fails() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let expected_filename_to_delete = Uuid::max().to_string();
            folder.given_folder_entries(
                "/tmp/tmp",
                vec![
                    Entry {
                        name: expected_filename_to_delete.clone(),
                        kind: file_system::EntryKind::File,
                        is_link: false,
                        size: 123,
                    },
                    Entry {
                        name: Uuid::nil().to_string(),
                        kind: file_system::EntryKind::Directory,
                        is_link: false,
                        size: 0,
                    },
                ],
            );
            file_deleter.given_delete_to_fail_once_with(
                &format!("/tmp/tmp/{expected_filename_to_delete}"),
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.purge();
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[test]
        fn test_should_delete_uuid_shaped_files() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let expected_filename_to_delete = Uuid::max().to_string();
            folder.given_folder_entries(
                "/tmp/tmp",
                vec![
                    Entry {
                        name: expected_filename_to_delete.clone(),
                        kind: file_system::EntryKind::File,
                        is_link: false,
                        size: 123,
                    },
                    Entry {
                        name: Uuid::nil().to_string(),
                        kind: file_system::EntryKind::Directory,
                        is_link: false,
                        size: 0,
                    },
                ],
            );
            file_deleter
                .expect_file_to_be_deleted(&format!("/tmp/tmp/{expected_filename_to_delete}"));

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.purge();
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn test_should_not_purge_active_files() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let temp_root = PathBuf::from("/tmp/tmp");
            let blob_root = PathBuf::from("/tmp/blobs");

            let state = Arc::new(Mutex::new(HashMap::<Uuid, PartialUpload>::new()));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();
            let expected_filename_to_delete = Uuid::max().to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{actual_sha}"
                ))
                .given_folder_entries(
                    "/tmp/tmp",
                    vec![
                        Entry {
                            name: expected_filename_to_delete.clone(),
                            kind: file_system::EntryKind::File,
                            is_link: false,
                            size: 123,
                        },
                        Entry {
                            name: Uuid::nil().to_string(),
                            kind: file_system::EntryKind::Directory,
                            is_link: false,
                            size: 0,
                        },
                    ],
                );

            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                temp_root,
                blob_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}").to_string());
            let uuid = blob_uploader.start("foo").expect("should start");
            blob_uploader
                .write_chunk(uuid, vec![0xC0, 0xDE].as_slice())
                .expect("should write");
            let actual = blob_uploader.purge();
            assert!(matches!(actual, Ok(())));
            blob_uploader
                .complete(Uuid::max(), &given_claimed, "mediatype")
                .expect("should complete")
        }
    }
}
