use crate::{
    blob_paths::{BlobCommit, BlobFilePaths},
    digest::{Digest, DigestError},
    name::{Name, NameParseError},
};
use file_system::{
    EntryKind, FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
};
use sha2::Sha256;
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    str::FromStr,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum BlobUploaderError {
    #[error("Invalid repository: '{0}'")]
    InvalidRespository(String),
    #[error("Unknown temporary file'{uuid}' for repository {name}")]
    UnknownUuid { uuid: Uuid, name: Name },
    #[error("Digest mismatch: claimed {claimed}, computed {computed}")]
    DigestMismatch { claimed: Digest, computed: Digest },
    #[error("File system error")]
    FileSystemError(#[from] FileSystemError),
    #[error("Digest error ")]
    DigestError(#[from] DigestError),
    #[error("Network error reading upload body: {0}")]
    NetworkError(String),
    #[error("Hash failure")]
    HashFailure,
    #[error("Range mismatch: expected offset {expected}, got {received}")]
    RangeMismatch { expected: u64, received: u64 },
    #[error("Name parse error: {0}")]
    NameParseError(#[from] NameParseError),
}

struct Paths {
    pub registry_root: PathBuf,
    pub temp_root: PathBuf,
}

impl Paths {
    pub fn new(root: &Path, registry: &Name) -> Self {
        let mut registry_root = root.to_path_buf();
        registry_root.push(registry.fs_safe());

        let mut temp_root = registry_root.clone();
        temp_root.push("tmp");

        Self {
            registry_root,
            temp_root,
        }
    }

    pub fn upload_temp_file(&self, uuid: Uuid) -> PathBuf {
        let mut result = self.temp_root.clone();
        result.push(uuid.to_string());

        result
    }
}

struct PartialUpload {
    temp_file: PathBuf,
    hasher: Option<Sha256>,
    offset: u64,
    file_deleter: Arc<dyn FileDeleter>,
    complete: bool,
}

impl PartialUpload {
    pub fn new(temp_file: &Path, file_deleter: Arc<dyn FileDeleter>) -> Self {
        Self {
            temp_file: temp_file.to_path_buf(),
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
            let _ = self.file_deleter.delete(&self.temp_file);
            self.complete = true
        }
    }
}

#[cfg_attr(test, mockall::automock)]
pub trait BlobUploader: Send + Sync {
    fn start(&self, registry: &Name) -> Result<Uuid, BlobUploaderError>;
    fn write_chunk<'a>(
        &'a self,
        registry: &Name,
        uuid: Uuid,
        expected_offset: u64,
        reader: Box<dyn AsyncRead + Send + Unpin>,
    ) -> BoxFuture<'a, Result<u64, BlobUploaderError>>;
    fn complete(
        &self,
        registry: &Name,
        uuid: Uuid,
        digest: &Digest,
        media_type: &str,
    ) -> Result<(), BlobUploaderError>;
    fn status(&self, registry: &Name, uuid: Uuid) -> Result<u64, BlobUploaderError>;
    fn abort(&self, registry: &Name, uuid: Uuid) -> Result<(), BlobUploaderError>;
    fn purge(&self) -> Result<(), BlobUploaderError>;
}

pub struct FileBlobUploader {
    repositories_root: PathBuf,
    state: Arc<Mutex<HashMap<Name, HashMap<Uuid, PartialUpload>>>>,
    folder: Arc<dyn Folder>,
    file_writer: Arc<dyn FileWriter>,
    file_appender: Arc<dyn FileAppender>,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    uuid_factory: Arc<dyn Fn() -> Uuid + Send + Sync>,
}

impl FileBlobUploader {
    pub fn new(
        repositories_root: PathBuf,
        folder: Arc<dyn Folder>,
        file_writer: Arc<dyn FileWriter>,
        file_appender: Arc<dyn FileAppender>,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
    ) -> Self {
        let state = Arc::new(Mutex::new(
            HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
        ));

        Self {
            repositories_root,
            state,
            folder,
            file_writer,
            file_appender,
            file_renamer,
            file_deleter,
            uuid_factory: Arc::new(Uuid::now_v7),
        }
    }

    fn purge_repository(
        &self,
        state: &std::sync::MutexGuard<'_, HashMap<Name, HashMap<Uuid, PartialUpload>>>,
        registry: &Name,
        temp_root: PathBuf,
    ) -> Result<(), BlobUploaderError> {
        let active_uploads = state.get(registry);

        for stale_entry in self.folder.entries(&temp_root)?.iter().filter(|entry| {
            entry.kind == EntryKind::File
                && match Uuid::parse_str(&entry.name) {
                    Ok(uuid) => !active_uploads.is_some_and(|uploads| uploads.contains_key(&uuid)),
                    Err(_) => false,
                }
        }) {
            let mut path = temp_root.clone();
            path.push(&stale_entry.name);
            self.file_deleter.delete(&path)?;
        }

        Ok(())
    }
}

impl BlobUploader for FileBlobUploader {
    fn start(&self, registry: &Name) -> Result<Uuid, BlobUploaderError> {
        let uuid = (self.uuid_factory)();

        let paths = Paths::new(&self.repositories_root, registry);
        self.folder.create_recursively(&paths.temp_root)?;

        self.state
            .lock()
            .unwrap()
            .entry(registry.clone())
            .or_default()
            .insert(
                uuid,
                PartialUpload::new(
                    &paths.upload_temp_file(uuid),
                    Arc::clone(&self.file_deleter),
                ),
            );

        Ok(uuid)
    }

    fn write_chunk<'a>(
        &'a self,
        registry: &Name,
        uuid: Uuid,
        expected_offset: u64,
        reader: Box<dyn AsyncRead + Send + Unpin>,
    ) -> BoxFuture<'a, Result<u64, BlobUploaderError>> {
        let name = registry.clone();

        Box::pin(async move {
            let (path, mut hasher, start_offset) = {
                let mut state = self.state.lock().unwrap();
                let upload = state
                    .get_mut(&name)
                    .and_then(|uploads| uploads.get_mut(&uuid))
                    .ok_or(BlobUploaderError::UnknownUuid {
                        uuid,
                        name: name.clone(),
                    })?;
                if expected_offset != upload.offset {
                    return Err(BlobUploaderError::RangeMismatch {
                        expected: upload.offset,
                        received: expected_offset,
                    });
                }
                let hasher = upload.hasher.take().ok_or(BlobUploaderError::HashFailure)?;
                (upload.temp_file.clone(), hasher, upload.offset)
            };

            let mut reader = reader;
            let mut buffer = vec![0u8; 65536];
            let mut written = 0u64;
            loop {
                let bytes_read = reader
                    .read(&mut buffer)
                    .await
                    .map_err(|err| BlobUploaderError::NetworkError(err.to_string()))?;
                if bytes_read == 0 {
                    break;
                }
                let chunk = &buffer[..bytes_read];
                self.file_appender.append_all_bytes(&path, chunk)?;
                sha2::digest::Update::update(&mut hasher, chunk);
                written += bytes_read as u64;
            }

            let mut state = self.state.lock().unwrap();
            match state
                .get_mut(&name)
                .and_then(|uploads| uploads.get_mut(&uuid))
            {
                Some(upload) => {
                    upload.hasher = Some(hasher);
                    upload.offset = start_offset + written;
                    Ok(start_offset + written)
                }
                None => Err(BlobUploaderError::HashFailure),
            }
        })
    }

    fn complete(
        &self,
        registry: &Name,

        uuid: Uuid,
        claimed: &Digest,
        media_type: &str,
    ) -> Result<(), BlobUploaderError> {
        let mut state = self.state.lock().unwrap();
        let paths = Paths::new(&self.repositories_root, registry);

        let Some(uploads) = state.get_mut(registry) else {
            return Err(BlobUploaderError::UnknownUuid {
                uuid,
                name: registry.clone(),
            });
        };
        let Some(mut upload) = uploads.remove(&uuid) else {
            return Err(BlobUploaderError::UnknownUuid {
                uuid,
                name: registry.clone(),
            });
        };
        if uploads.is_empty() {
            state.remove(registry);
        }

        let actual = sha2::Digest::finalize(upload.hasher.take().unwrap());
        let computed = actual.as_slice();
        if claimed.as_bytes().as_slice() != computed {
            return Err(BlobUploaderError::DigestMismatch {
                claimed: claimed.clone(),
                computed: Digest::from_bytes(&computed)?,
            });
        }

        let blob_paths = BlobFilePaths::new(&paths.registry_root, claimed);
        self.folder.create_recursively(&blob_paths.final_root)?;

        let mut commit = BlobCommit::new(
            &upload.temp_file,
            &blob_paths,
            Arc::clone(&self.file_renamer),
            Arc::clone(&self.file_deleter),
            Arc::clone(&self.file_writer),
        );
        upload.mark_complete();
        commit.complete(media_type)?;

        Ok(())
    }

    fn abort(&self, registry: &Name, uuid: Uuid) -> Result<(), BlobUploaderError> {
        let mut state = self.state.lock().unwrap();

        let Some(uploads) = state.get_mut(registry) else {
            return Err(BlobUploaderError::UnknownUuid {
                uuid,
                name: registry.clone(),
            });
        };
        let Some(upload) = uploads.remove(&uuid) else {
            return Err(BlobUploaderError::UnknownUuid {
                uuid,
                name: registry.clone(),
            });
        };
        if uploads.is_empty() {
            state.remove(registry);
        }
        drop(upload);
        Ok(())
    }

    fn purge(&self) -> Result<(), BlobUploaderError> {
        let state = self.state.lock().unwrap();

        for registry in self
            .folder
            .entries(&self.repositories_root)?
            .iter()
            .filter_map(|entry| {
                if entry.kind != EntryKind::Directory {
                    return None;
                }
                Name::from_str(&entry.name).ok()
            })
        {
            self.purge_repository(
                &state,
                &registry,
                Paths::new(&self.repositories_root, &registry).temp_root,
            )?;
        }

        Ok(())
    }

    fn status(&self, registry: &Name, uuid: Uuid) -> Result<u64, BlobUploaderError> {
        let state = self.state.lock().unwrap();

        match state.get(registry).and_then(|uploads| uploads.get(&uuid)) {
            Some(upload) => Ok(upload.offset),
            None => Err(BlobUploaderError::UnknownUuid {
                uuid,
                name: registry.clone(),
            }),
        }
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
                &PathBuf::from("/tmp/file"),
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
            );

            drop(upload);
        }

        #[test]
        fn test_should_not_delete_if_complete() {
            let file_deleter = MockFileDeleter::new();

            let mut upload = PartialUpload::new(
                &PathBuf::from("/tmp/file"),
                Arc::new(file_deleter) as Arc<dyn FileDeleter>,
            );

            upload.mark_complete();

            drop(upload);
        }
    }

    mod start {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            name::Name,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use mockall::predicate;
        use std::{
            collections::HashMap,
            path::PathBuf,
            str::FromStr,
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
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            folder
                .expect_create_recursively()
                .with(predicate::eq(PathBuf::from("/tmp/foo/tmp")))
                .returning(|_| Err(FileSystemError::ExpectedFileError));

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start(&registry);
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
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(&format!("/tmp/foo/tmp/{}", Uuid::max()));
            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start(&registry);
            assert!(matches!(
                actual,
                Ok(uuid) if uuid == Uuid::max()));
        }
    }

    mod write_chunk {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            name::Name,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            io::Cursor,
            path::PathBuf,
            str::FromStr,
            sync::{Arc, Mutex},
        };
        use uuid::Uuid;

        #[tokio::test]
        async fn test_write_chunk_should_fail_for_uuids_not_started() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader
                .write_chunk(&registry, Uuid::max(), 0, Box::new(Cursor::new(Vec::new())))
                .await;
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }

        #[tokio::test]
        async fn test_should_fail_if_cannot_append_when_write_chunk() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");

            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            file_appender.given_append_all_bytes_fails_once_with(
                temp_file,
                vec![0xDE, 0xAD, 0xBE, 0xEF],
                FileSystemError::ExpectedFileError,
            );
            file_deleter.expect_file_to_be_deleted(temp_file);

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start(&registry);
            assert!(matches!(
                actual,
                Ok(uuid) if uuid == Uuid::max()));
            let actual = blob_uploader
                .write_chunk(
                    &registry,
                    Uuid::max(),
                    0,
                    Box::new(Cursor::new(vec![0xDE, 0xAD, 0xBE, 0xEF])),
                )
                .await;
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[tokio::test]
        async fn test_should_fail_with_range_mismatch_when_offset_is_wrong() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            file_deleter.expect_file_to_be_deleted(temp_file);

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            blob_uploader.start(&registry).unwrap();
            let actual = blob_uploader
                .write_chunk(
                    &registry,
                    Uuid::max(),
                    42,
                    Box::new(Cursor::new(vec![0xDE, 0xAD])),
                )
                .await;
            assert!(matches!(
                actual,
                Err(BlobUploaderError::RangeMismatch {
                    expected: 0,
                    received: 42
                })
            ));
        }

        #[tokio::test]
        async fn test_should_write_chunk() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            file_appender.expect_append_all_bytes_with(temp_file, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            file_deleter.expect_file_to_be_deleted(temp_file);

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.start(&registry);
            assert!(matches!(
                actual,
                Ok(uuid) if uuid == Uuid::max()));
            let actual = blob_uploader
                .write_chunk(
                    &registry,
                    Uuid::max(),
                    0,
                    Box::new(Cursor::new(vec![0xDE, 0xAD, 0xBE, 0xEF])),
                )
                .await;
            assert!(matches!(actual, Ok(4)));
        }
    }

    mod complete {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            digest::Digest,
            name::Name,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            io::Cursor,
            path::PathBuf,
            str::FromStr,
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
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let claimed = format!("sha256:{}", "f".repeat(64));
            let actual =
                blob_uploader.complete(&registry, Uuid::max(), &Digest(claimed), "mediatype");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }

        #[tokio::test]
        async fn test_should_return_error_if_digest_does_not_match() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{}", "f".repeat(64)));
            let expected_computed = Digest(
                "sha256:1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf"
                    .to_string(),
            );
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");

            let actual =
                blob_uploader.complete(&registry, Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                    actual,
                    Err(BlobUploaderError::DigestMismatch { claimed, computed })
                    if claimed == given_claimed && computed == expected_computed));
        }

        #[tokio::test]
        async fn test_should_return_error_and_clean_up_if_rename_fails() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_deleter.expect_file_to_be_deleted(&format!(
                "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"
            ));
            file_deleter.expect_file_to_be_deleted(&format!(
                "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"
            ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.given_rename_fails_once_with(
                temp_file,
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");

            let actual =
                blob_uploader.complete(&registry, Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[tokio::test]
        async fn test_should_return_error_and_clean_up_if_sidecar_fails() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_deleter.expect_file_to_be_deleted(temp_file);
            file_deleter.expect_file_to_be_deleted(&format!(
                "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"
            ));
            file_deleter.expect_file_to_be_deleted(&format!(
                "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"
            ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.given_write_to_file_fails_once_with(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");

            let actual =
                blob_uploader.complete(&registry, Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(
                actual,
                Err(BlobUploaderError::FileSystemError(
                    FileSystemError::ExpectedFileError
                ))
            ));
        }

        #[tokio::test]
        async fn test_should_complete() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");

            let actual =
                blob_uploader.complete(&registry, Uuid::max(), &given_claimed, "mediatype");
            assert!(matches!(actual, Ok(())));
        }

        #[tokio::test]
        async fn test_should_remove_registry_entry_when_last_upload_completes() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");
            blob_uploader
                .complete(&registry, Uuid::max(), &given_claimed, "mediatype")
                .expect("should complete");

            assert!(!blob_uploader.state.lock().unwrap().contains_key(&registry));
        }

        #[tokio::test]
        async fn test_should_keep_registry_entry_when_other_uploads_remain() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            let first_uuid = Uuid::nil();
            let second_uuid = Uuid::max();
            let remaining = Mutex::new(std::collections::VecDeque::from([first_uuid, second_uuid]));
            let uuid_factory = Arc::new(move || remaining.lock().unwrap().pop_front().unwrap());

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(
                &format!("/tmp/foo/tmp/{first_uuid}"),
                vec![0xC0, 0xDE],
            );
            file_renamer.expect_rename_with(
                &format!("/tmp/foo/tmp/{first_uuid}"),
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );
            file_deleter.expect_file_to_be_deleted(&format!("/tmp/foo/tmp/{second_uuid}"));

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let first = blob_uploader.start(&registry).expect("should start first");
            let second = blob_uploader.start(&registry).expect("should start second");
            assert_eq!(first, first_uuid);
            assert_eq!(second, second_uuid);

            blob_uploader
                .write_chunk(&registry, first, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");
            blob_uploader
                .complete(&registry, first, &given_claimed, "mediatype")
                .expect("should complete");

            let state = blob_uploader.state.lock().unwrap();
            let uploads = state.get(&registry).expect("registry entry should remain");
            assert!(uploads.contains_key(&second));
        }

        #[tokio::test]
        async fn test_should_error_with_additional_writes_after_complete() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);

            file_renamer.expect_rename_with(
                temp_file,
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");

            blob_uploader
                .complete(&registry, Uuid::max(), &given_claimed, "mediatype")
                .expect("should complete");
            let actual = blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await;
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }
    }

    mod abort {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            name::Name,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileWriter, Folder, MockFileAppender,
            MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            path::PathBuf,
            str::FromStr,
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
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.abort(&registry, Uuid::max());
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }

        #[test]
        fn test_should_clean_up() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start(&registry).expect("should start");
            let actual = blob_uploader.abort(&registry, uuid);
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn test_should_remove_registry_entry_when_last_upload_aborts() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader.abort(&registry, uuid).expect("should abort");

            assert!(!blob_uploader.state.lock().unwrap().contains_key(&registry));
        }

        #[test]
        fn test_should_keep_registry_entry_when_other_uploads_remain() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let first_uuid = Uuid::nil();
            let second_uuid = Uuid::max();
            let remaining = Mutex::new(std::collections::VecDeque::from([first_uuid, second_uuid]));
            let uuid_factory = Arc::new(move || remaining.lock().unwrap().pop_front().unwrap());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(&format!("/tmp/foo/tmp/{first_uuid}"));
            file_deleter.expect_file_to_be_deleted(&format!("/tmp/foo/tmp/{second_uuid}"));

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let first = blob_uploader.start(&registry).expect("should start first");
            let second = blob_uploader.start(&registry).expect("should start second");
            assert_eq!(first, first_uuid);
            assert_eq!(second, second_uuid);

            blob_uploader
                .abort(&registry, first)
                .expect("should abort first");

            let state = blob_uploader.state.lock().unwrap();
            let uploads = state.get(&registry).expect("registry entry should remain");
            assert!(uploads.contains_key(&second));
        }

        #[test]
        fn test_should_not_double_clean_up() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .abort(&registry, uuid)
                .expect("should work the first time");
            let actual = blob_uploader.abort(&registry, Uuid::max());
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }
    }

    mod purge {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            digest::Digest,
            name::Name,
        };
        use file_system::{
            Entry, FileAppender, FileDeleter, FileRenamer, FileSystemError, FileWriter, Folder,
            MockFileAppender, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            io::Cursor,
            path::PathBuf,
            str::FromStr,
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
            let repositories_root = PathBuf::from("/tmp");

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let expected_filename_to_delete = Uuid::max().to_string();
            folder.given_folder_entries(
                "/tmp",
                vec![Entry {
                    name: "foo".to_string(),
                    kind: file_system::EntryKind::Directory,
                    is_link: false,
                    size: 0,
                }],
            );
            folder.given_folder_entries(
                "/tmp/foo/tmp",
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
                &format!("/tmp/foo/tmp/{expected_filename_to_delete}"),
                FileSystemError::ExpectedFileError,
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
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
            let repositories_root = PathBuf::from("/tmp");

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let expected_filename_to_delete = Uuid::max().to_string();
            folder.given_folder_entries(
                "/tmp",
                vec![Entry {
                    name: "foo".to_string(),
                    kind: file_system::EntryKind::Directory,
                    is_link: false,
                    size: 0,
                }],
            );
            folder.given_folder_entries(
                "/tmp/foo/tmp",
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
                .expect_file_to_be_deleted(&format!("/tmp/foo/tmp/{expected_filename_to_delete}"));

            let blob_uploader = FileBlobUploader {
                repositories_root,
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

        #[tokio::test]
        async fn test_should_not_purge_active_files() {
            let mut folder = MockFolder::new();
            let mut file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let mut file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());
            let actual_sha = "1b96011418a3675a82b529695daac30914827d65d2ff3e0bc6873526a1beefcf";
            let prefix = actual_sha[0..2].to_string();
            let active_uuid = Uuid::max().to_string();

            folder
                .expect_create_folder_recursively_with("/tmp/foo/tmp")
                .expect_create_folder_recursively_with(&format!(
                    "/tmp/foo/blobs/sha256/{prefix}/{actual_sha}"
                ));
            folder.given_folder_entries(
                "/tmp",
                vec![Entry {
                    name: "foo".to_string(),
                    kind: file_system::EntryKind::Directory,
                    is_link: false,
                    size: 0,
                }],
            );
            folder.given_folder_entries(
                "/tmp/foo/tmp",
                vec![
                    Entry {
                        name: active_uuid,
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
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}"),
            );
            file_writer.expect_write_to_file_with_contents(
                &format!("/tmp/foo/blobs/sha256/{prefix}/{actual_sha}/{actual_sha}.mediatype"),
                "mediatype",
            );

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let given_claimed = Digest(format!("sha256:{actual_sha}"));
            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");
            let actual = blob_uploader.purge();
            assert!(matches!(actual, Ok(())));
            blob_uploader
                .complete(&registry, Uuid::max(), &given_claimed, "mediatype")
                .expect("should complete")
        }
    }

    mod status {
        use crate::{
            blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader, PartialUpload},
            name::Name,
        };
        use file_system::{
            FileAppender, FileDeleter, FileRenamer, FileWriter, Folder, MockFileAppender,
            MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder,
        };
        use std::{
            collections::HashMap,
            io::Cursor,
            path::PathBuf,
            str::FromStr,
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
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let actual = blob_uploader.status(&registry, Uuid::max());
            assert!(matches!(
                actual,
                Err(BlobUploaderError::UnknownUuid { uuid, name })
                    if uuid == Uuid::max() && name == registry
            ));
        }

        #[tokio::test]
        async fn test_should_return_offset() {
            let mut folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_appender = MockFileAppender::new();
            let file_renamer = MockFileRenamer::new();
            let mut file_deleter = MockFileDeleter::new();
            let repositories_root = PathBuf::from("/tmp");
            let registry = Name::from_str("foo").unwrap();

            let state = Arc::new(Mutex::new(
                HashMap::<Name, HashMap<Uuid, PartialUpload>>::new(),
            ));
            let uuid_factory = Arc::new(Uuid::max);
            let temp_file = &format!("/tmp/foo/tmp/{}", Uuid::max());

            folder.expect_create_folder_recursively_with("/tmp/foo/tmp");
            file_appender.expect_append_all_bytes_with(temp_file, vec![0xC0, 0xDE]);
            file_deleter.expect_file_to_be_deleted(temp_file);

            let blob_uploader = FileBlobUploader {
                repositories_root,
                folder: Arc::new(folder) as Arc<dyn Folder>,
                file_writer: Arc::new(file_writer) as Arc<dyn FileWriter>,
                file_appender: Arc::new(file_appender) as Arc<dyn FileAppender>,
                file_renamer: Arc::new(file_renamer) as Arc<dyn FileRenamer>,
                file_deleter: Arc::new(file_deleter) as Arc<dyn FileDeleter>,
                uuid_factory,
                state,
            };

            let uuid = blob_uploader.start(&registry).expect("should start");
            blob_uploader
                .write_chunk(&registry, uuid, 0, Box::new(Cursor::new(vec![0xC0, 0xDE])))
                .await
                .expect("should write");
            let actual = blob_uploader.status(&registry, uuid);
            assert!(matches!(actual, Ok(2)));
        }
    }
}
