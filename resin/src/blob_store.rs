use crate::{
    blob_paths::{BlobCommit, BlobFilePaths},
    digest,
};
use file_system::{
    EntryKind, FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder, Inspect,
};
use sha2::Sha256;
use std::{future::Future, path::Path, pin::Pin, sync::Arc};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum BlobError {
    #[error("Digest not found {0}")]
    DigestNotFound(String),
    #[error("Digest error")]
    DigestError(#[from] crate::digest::DigestError),
    #[error("Digest mismatch: claimed {claimed}, computed {computed}")]
    DigestMismatch {
        claimed: digest::Digest,
        computed: digest::Digest,
    },
    #[error("IO Error")]
    IoError(#[from] std::io::Error),
    #[error("File system error")]
    FileSystemError(#[from] FileSystemError),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Stats {
    pub size: u64,
    pub mediatype: String,
}

pub trait BlobStore: Send + Sync {
    fn save<'a>(
        &'a self,
        blob_root: &'a Path,
        claimed: &'a digest::Digest,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        mediatype: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>>;

    fn get<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin>, BlobError>>;

    fn exists<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<bool, BlobError>>;

    fn stats<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<Stats, BlobError>>;

    fn delete<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<(), BlobError>>;
}

pub struct FileBlobStore {
    folder: Arc<dyn Folder>,
    file_writer: Arc<dyn FileWriter>,
    file_reader: Arc<dyn FileReader>,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    inspect: Arc<dyn Inspect>,
}

impl FileBlobStore {
    pub fn new(
        folder: Arc<dyn Folder>,
        file_writer: Arc<dyn FileWriter>,
        file_reader: Arc<dyn FileReader>,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
        inspect: Arc<dyn Inspect>,
    ) -> Self {
        Self {
            folder,
            file_writer,
            file_reader,
            file_renamer,
            file_deleter,
            inspect,
        }
    }

    async fn hashed_read_to_temp_file(
        &self,
        mut source: impl AsyncRead + Send + Unpin,
        temp_file: &Path,
    ) -> Result<sha2::digest::Output<Sha256>, BlobError> {
        let mut hasher = <Sha256 as sha2::Digest>::new();
        let mut buffer = [0; 1024 * 64];
        let mut writer = self
            .file_writer
            .create_buffered_file_writer(temp_file, Arc::clone(&self.file_deleter))?;

        loop {
            let mut bytes_read = 0;
            while bytes_read < buffer.len() {
                let read = match source.read(&mut buffer[bytes_read..]).await {
                    Ok(read) => read,
                    Err(err) => {
                        return Err(BlobError::IoError(err));
                    }
                };

                if read == 0 {
                    break;
                }
                bytes_read += read;
            }

            if bytes_read == 0 {
                break;
            }

            sha2::digest::Update::update(&mut hasher, &buffer[0..bytes_read]);
            if let Err(err) = writer.write_all(&buffer[0..bytes_read]) {
                return Err(BlobError::FileSystemError(err));
            }
        }
        writer.close();
        let computed = sha2::Digest::finalize(hasher);

        Ok(computed)
    }

    fn create_unverified_file<'a>(
        &self,
        blob_root: &'a Path,
        claimed: &'a digest::Digest,
    ) -> Result<BlobCommit, FileSystemError> {
        let paths = BlobFilePaths::new(blob_root, claimed);
        self.folder.create_recursively(&paths.final_root)?;

        let mut temp_file = paths.final_root.clone();
        temp_file.push(format!("{}.tmp", claimed.hex()));

        Ok(BlobCommit::new(
            &temp_file,
            &paths,
            Arc::clone(&self.file_renamer),
            Arc::clone(&self.file_deleter),
            Arc::clone(&self.file_writer),
        ))
    }
}

impl BlobStore for FileBlobStore {
    fn save<'a>(
        &'a self,
        blob_root: &'a Path,
        claimed: &'a digest::Digest,
        source: Box<dyn AsyncRead + Send + Unpin>,
        mediatype: &'a str,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            if self.exists(blob_root, claimed).await? {
                return Ok(());
            }

            let mut unverified_file = self.create_unverified_file(blob_root, claimed)?;

            let hash = self
                .hashed_read_to_temp_file(source, unverified_file.temp_file())
                .await?;
            let computed = hash.as_slice();

            if claimed.as_bytes().as_slice() != computed {
                unverified_file.clean_up()?;
                return Err(BlobError::DigestMismatch {
                    claimed: claimed.clone(),
                    computed: digest::Digest::from_bytes(&computed)?,
                });
            }

            unverified_file.complete(mediatype)?;

            Ok(())
        })
    }

    fn exists<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<bool, BlobError>> {
        Box::pin(async move {
            let paths = BlobFilePaths::new(blob_root, digest);
            Ok(self.inspect.exists(&paths.final_file))
        })
    }

    fn get<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin>, BlobError>> {
        Box::pin(async move {
            let paths = BlobFilePaths::new(blob_root, digest);
            let result = self.file_reader.create_reader(&paths.final_file)?;
            Ok(result)
        })
    }

    fn stats<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<Stats, BlobError>> {
        Box::pin(async move {
            let paths = BlobFilePaths::new(blob_root, digest);
            if self.inspect.exists(&paths.final_file) {
                let entry = self.inspect.read_metadata(&paths.final_file)?;
                let mediatype = self
                    .file_reader
                    .read_all(&paths.mediatype_file)?
                    .trim()
                    .to_string();

                Ok(Stats {
                    size: entry.size,
                    mediatype,
                })
            } else {
                Err(BlobError::DigestNotFound(digest.to_string()))
            }
        })
    }

    fn delete<'a>(
        &'a self,
        blob_root: &'a Path,
        digest: &'a digest::Digest,
    ) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let paths = BlobFilePaths::new(blob_root, digest);
            if self.inspect.exists(&paths.final_file) {
                for entry in self
                    .folder
                    .entries(&paths.final_root)?
                    .iter()
                    .filter(|entry| entry.kind == EntryKind::File)
                {
                    let mut to_delete = paths.final_root.clone();
                    to_delete.push(&entry.name);
                    self.file_deleter.delete(&to_delete)?;
                }

                Ok(())
            } else {
                Err(BlobError::DigestNotFound(digest.to_string()))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod docker_digest {
        use crate::digest::{self, DigestError};
        use std::str::FromStr;

        #[test]
        fn test_should_trim_non_hex() {
            let digest = digest::Digest("sha256:baadf00d".to_string());
            let actual = digest.hex();
            assert_eq!("baadf00d".to_string(), actual);
        }

        #[test]
        fn test_should_create_byte_stream() {
            let digest = digest::Digest("sha256:baadf00d".to_string());
            let actual = digest.as_bytes();
            assert_eq!(vec![0xba, 0xad, 0xf0, 0x0d], actual);
        }

        #[test]
        fn test_should_create_str() {
            let digest = digest::Digest("sha256:baadf00d".to_string());
            let actual = digest.as_str();
            assert_eq!("sha256:baadf00d", actual);
        }

        #[test]
        fn test_should_err_if_byte_stream_too_short() {
            let digest = digest::Digest::from_bytes(&vec![0xba, 0xad, 0xf0, 0x0d]);

            assert!(matches!(digest, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_err_if_byte_stream_too_long() {
            let data = [0xFF; 33];
            let actual = digest::Digest::from_bytes(&data);

            assert!(matches!(actual, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_create_from_byte_stream() {
            let data = [0xFF; 32];

            let sha = "ff".repeat(32);
            let expected = digest::Digest(format!("sha256:{sha}"));
            let actual = digest::Digest::from_bytes(&data);

            assert!(matches!(actual, Ok(d) if d == expected));
        }

        #[test]
        fn test_should_reject_unprefixed_values() {
            let result = digest::Digest::from_str("oops:baadf00d");
            assert!(matches!(result, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_reject_short_values() {
            let result = digest::Digest::from_str("sha256:baadf00d");
            assert!(matches!(result, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_reject_invalid_values() {
            let sha = "q".repeat(64);
            let result = digest::Digest::from_str(&format!("sha256:{sha}"));
            assert!(matches!(result, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_reject_capital_values() {
            let sha = "F".repeat(64);
            let result = digest::Digest::from_str(&format!("sha256:{sha}"));
            assert!(matches!(result, Err(DigestError::InvalidDigest)));
        }

        #[test]
        fn test_should_convert_from_str() {
            let sha = "f".repeat(64);
            let result = digest::Digest::from_str(&format!("sha256:{sha}"));
            assert!(matches!(result, Ok(d) if d == digest::Digest(format!("sha256:{sha}"))));
        }

        #[test]
        fn test_round_trips() {
            let sha = "f".repeat(64);
            let string = format!("sha256:{sha}");
            let digest: digest::Digest = string.parse().unwrap();
            assert_eq!(digest.as_str(), string);
        }
    }

    mod blob_store {
        mod save {
            use crate::blob_store::{BlobError, BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                FileSystemError, MockBufferedFileWiter, MockFileDeleter, MockFileReader,
                MockFileRenamer, MockFileWriter, MockFolder, MockInspect,
            };
            use mockall::predicate;
            use std::{path::PathBuf, sync::Arc};

            #[derive(Debug)]
            struct FailingReader;

            impl tokio::io::AsyncRead for FailingReader {
                fn poll_read(
                    self: std::pin::Pin<&mut Self>,
                    _cx: &mut std::task::Context<'_>,
                    _buf: &mut tokio::io::ReadBuf<'_>,
                ) -> std::task::Poll<std::io::Result<()>> {
                    std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "simulated read error",
                    )))
                }
            }

            #[tokio::test]
            async fn test_should_fail_if_working_dir_could_not_be_created() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                inspect.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder
                    .expect_create_recursively()
                    .with(predicate::eq(PathBuf::from("/tmp/blobs/sha256/f0/f00d")))
                    .returning(|_| Err(FileSystemError::ExpectedFileError));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(Vec::new());
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;
                assert!(matches!(
                    result,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_fail_if_temp_file_could_not_be_created() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                inspect.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer
                    .expect_write_all()
                    .returning(|_| Err(FileSystemError::ExpectedFileError));

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.mediatype");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(
                    result,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_fail_if_reader_fails() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                inspect.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");

                let buffered_writer = MockBufferedFileWiter::new();

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.mediatype");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = FailingReader {};
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(
                    result,
                    Err(BlobError::IoError(e)) if e.kind() == std::io::ErrorKind::BrokenPipe
                ));
            }

            #[tokio::test]
            async fn test_should_fail_digests_dont_match() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                inspect.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");
                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));
                buffered_writer.expect_close().once().return_const(());

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.mediatype");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(result, Err(BlobError::DigestMismatch { .. })));
            }

            #[tokio::test]
            async fn test_should_fail_if_rename_failed() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let mut file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                inspect.given_does_not_exist(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"
                ));
                folder.expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}"
                ));

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));
                buffered_writer.expect_close().once().return_const(());

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                let expected_from_path = PathBuf::from(format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}.tmp"
                ));
                let expected_to_path =
                    PathBuf::from(format!("/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"));

                file_renamer
                    .expect_rename()
                    .with(
                        predicate::eq(expected_from_path),
                        predicate::eq(expected_to_path),
                    )
                    .returning(|_, _| Err(FileSystemError::ExpectedFileError));

                file_deleter.expect_file_to_be_deleted(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}.tmp"
                ));
                file_deleter.expect_file_to_be_deleted(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"
                ));
                file_deleter.expect_file_to_be_deleted(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}.mediatype"
                ));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(
                    result,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_not_overwrite_write() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                inspect.given_exists(&format!("/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}/"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(result, Ok(())))
            }

            #[tokio::test]
            async fn test_should_write_blob() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let mut file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                inspect.given_does_not_exist(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"
                ));
                folder.expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}"
                ));

                file_writer.expect_write_to_file_with_contents(
                    &format!("/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}.mediatype"),
                    "mediatype",
                );

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));
                buffered_writer.expect_close().once().return_const(());

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                let expected_from_path = PathBuf::from(format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}.tmp"
                ));
                let expected_to_path =
                    PathBuf::from(format!("/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"));

                file_renamer
                    .expect_rename()
                    .with(
                        predicate::eq(expected_from_path),
                        predicate::eq(expected_to_path),
                    )
                    .returning(|_, _| Ok(()));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store
                    .save(&blob_root, &digest, Box::new(source), "mediatype")
                    .await;

                assert!(matches!(result, Ok(())))
            }
        }

        mod exists {
            use crate::blob_store::{BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                MockFileDeleter, MockFileReader, MockFileRenamer, MockFileWriter, MockFolder,
                MockInspect,
            };
            use std::{path::PathBuf, sync::Arc};

            #[tokio::test]
            async fn test_should_determine_if_blob_does_not_exist() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let sha = "ff".repeat(32);
                inspect.given_does_not_exist(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );
                let actual = store
                    .exists(&blob_root, &digest::Digest(format!("sha256:{sha}")))
                    .await;
                assert!(matches!(actual, Ok(false)));
            }

            #[tokio::test]
            async fn test_should_determine_if_blob_does_exist() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let sha = "ff".repeat(32);
                inspect.given_exists(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );
                let actual = store
                    .exists(&blob_root, &digest::Digest(format!("sha256:{sha}")))
                    .await;

                assert!(matches!(actual, Ok(true)));
            }
        }

        mod get {
            use crate::blob_store::{BlobError, BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                FileSystemError, MockFileDeleter, MockFileReader, MockFileRenamer, MockFileWriter,
                MockFolder, MockInspect,
            };
            use mockall::predicate;
            use std::{path::PathBuf, sync::Arc};

            #[tokio::test]
            async fn test_should_fail_if_no_blob() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let mut file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let sha = "ff".repeat(32);

                let expected_file = PathBuf::from(format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                file_reader
                    .expect_create_reader()
                    .with(predicate::eq(expected_file.clone()))
                    .returning(move |_| {
                        Err(file_system::FileSystemError::NotFoundError(
                            expected_file.clone(),
                        ))
                    });

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );
                let actual = store
                    .get(&blob_root, &digest::Digest(format!("sha256:{sha}")))
                    .await;
                let expected_missing_file_path =
                    PathBuf::from(format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                assert!(matches!(
                    actual,
                    Err(BlobError::FileSystemError(
                        FileSystemError::NotFoundError(f)
                    )) if f == expected_missing_file_path
                ));
            }

            #[tokio::test]
            async fn test_should_return_reader() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let mut file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");

                let sha = "ff".repeat(32);

                let expected_file = PathBuf::from(format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                file_reader
                    .expect_create_reader()
                    .with(predicate::eq(expected_file.clone()))
                    .returning(move |_| Ok(Box::new(std::io::Cursor::new(Vec::new()))));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );
                let actual = store
                    .get(&blob_root, &digest::Digest(format!("sha256:{sha}")))
                    .await;
                assert!(actual.is_ok());
            }
        }

        mod delete {
            use crate::blob_store::{BlobError, BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                Entry, FileSystemError, MockFileDeleter, MockFileReader, MockFileRenamer,
                MockFileWriter, MockFolder, MockInspect,
            };
            use mockall::predicate;
            use std::{path::PathBuf, sync::Arc};

            #[tokio::test]
            async fn test_should_fail_if_blob_does_not_exist() {
                let folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");
                let sha = "ff".repeat(32);

                inspect.given_does_not_exist(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let request = digest::Digest(format!("sha256:{sha}"));
                let actual = store.delete(&blob_root, &request).await;
                assert!(matches!(
                    actual,
                    Err(BlobError::DigestNotFound(digest)) if digest == request.to_string()
                ));
            }

            #[tokio::test]
            async fn test_should_fail_if_entries_cannot_be_listed() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");
                let sha = "ff".repeat(32);

                inspect.given_exists(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                folder
                    .expect_entries()
                    .with(predicate::eq(PathBuf::from(format!(
                        "/tmp/blobs/sha256/ff/{sha}"
                    ))))
                    .returning(|_| Err(FileSystemError::ExpectedFileError));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let request = digest::Digest(format!("sha256:{sha}"));
                let actual = store.delete(&blob_root, &request).await;
                assert!(matches!(
                    actual,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_fail_if_file_deletion_fails() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");
                let sha = "ff".repeat(32);

                inspect.given_exists(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                folder.given_folder_entries(
                    &format!("/tmp/blobs/sha256/ff/{sha}"),
                    vec![Entry::create_file_entry(&sha)],
                );

                file_deleter.given_delete_to_fail_once_with(
                    &format!("/tmp/blobs/sha256/ff/{sha}/{sha}"),
                    FileSystemError::ExpectedFileError,
                );

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let request = digest::Digest(format!("sha256:{sha}"));
                let actual = store.delete(&blob_root, &request).await;
                assert!(matches!(
                    actual,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_delete_blob() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let mut inspect = MockInspect::new();
                let blob_root = PathBuf::from("/tmp");
                let sha = "ff".repeat(32);

                inspect.given_exists(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                folder.given_folder_entries(
                    &format!("/tmp/blobs/sha256/ff/{sha}"),
                    vec![
                        Entry::create_file_entry(&sha),
                        Entry::create_file_entry(&format!("{sha}.mediatype")),
                        Entry::create_directory("whoops"),
                    ],
                );

                file_deleter
                    .expect_file_to_be_deleted(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));
                file_deleter.expect_file_to_be_deleted(&format!(
                    "/tmp/blobs/sha256/ff/{sha}/{sha}.mediatype"
                ));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    Arc::new(inspect),
                );

                let request = digest::Digest(format!("sha256:{sha}"));
                let actual = store.delete(&blob_root, &request).await;
                assert!(matches!(actual, Ok(())));
            }
        }

        mod round_trip {
            use crate::blob_store::{BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                BufferedFileWiter, Entry, EntryKind, FileDeleter, FileReader, FileRenamer,
                FileSystemError, FileWriter, Folder, Inspect, RelativePath,
            };
            use std::collections::HashMap;
            use std::path::{Path, PathBuf};
            use std::sync::{Arc, Mutex};
            use tokio::io::AsyncReadExt;

            #[derive(Clone)]
            struct FakeDisk(Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>);

            impl FakeDisk {
                fn new() -> Self {
                    Self(Arc::new(Mutex::new(HashMap::new())))
                }
            }

            impl Folder for FakeDisk {
                fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
                    Ok(path.to_path_buf())
                }
                fn create_recursively(&self, path: &Path) -> Result<PathBuf, FileSystemError> {
                    Ok(path.to_path_buf())
                }
                fn entries(&self, _: &Path) -> Result<Vec<Entry>, FileSystemError> {
                    Ok(vec![])
                }
                fn exists(&self, path: &Path) -> bool {
                    self.0.lock().unwrap().contains_key(path)
                }
                fn pop(&self, _: &Path) -> Option<String> {
                    None
                }
                fn executable_root(&self) -> Result<PathBuf, FileSystemError> {
                    Err(FileSystemError::ExpectedFileError)
                }
                fn create_file(&self, _: &Path) -> Result<std::fs::File, FileSystemError> {
                    Err(FileSystemError::ExpectedFileError)
                }
                fn open_file_for_writing(
                    &self,
                    _: &Path,
                ) -> Result<std::fs::File, FileSystemError> {
                    Err(FileSystemError::ExpectedFileError)
                }
                fn parent(&self, path: &Path) -> Option<PathBuf> {
                    path.parent().map(|p| p.to_path_buf())
                }
                fn combine(&self, left: &Path, right: &Path) -> PathBuf {
                    left.join(right)
                }
                fn split(&self, path: &Path) -> Vec<PathBuf> {
                    path.ancestors().map(|p| p.to_path_buf()).collect()
                }
                fn create_relative_path(
                    &self,
                    root: &Path,
                    child: &Path,
                ) -> Result<RelativePath, FileSystemError> {
                    let relative =
                        child
                            .strip_prefix(root)
                            .map_err(|_| FileSystemError::NotChild {
                                root: root.to_path_buf(),
                                child: child.to_path_buf(),
                            })?;

                    RelativePath::try_from(relative.to_path_buf())
                        .map_err(|_| FileSystemError::InvalidPath(child.to_path_buf()))
                }
            }

            impl Inspect for FakeDisk {
                fn is_directory(&self, _: &Path) -> bool {
                    false
                }
                fn read_metadata(&self, path: &Path) -> Result<Entry, FileSystemError> {
                    let disk = self.0.lock().unwrap();
                    let size = disk
                        .get(path)
                        .map(|v| v.len() as u64)
                        .ok_or(FileSystemError::ExpectedFileError)?;
                    Ok(Entry {
                        name: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        path: path.to_path_buf(),
                        kind: EntryKind::File,
                        is_link: false,
                        size,
                    })
                }
                fn exists(&self, path: &Path) -> bool {
                    self.0.lock().unwrap().contains_key(path)
                }
            }

            struct FakeBufferedWriter {
                path: PathBuf,
                disk: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
            }

            impl BufferedFileWiter for FakeBufferedWriter {
                fn write_all(&mut self, bytes: &[u8]) -> Result<(), FileSystemError> {
                    self.disk
                        .lock()
                        .unwrap()
                        .entry(self.path.clone())
                        .or_default()
                        .extend_from_slice(bytes);
                    Ok(())
                }
                fn close(&mut self) {}
            }

            impl FileWriter for FakeDisk {
                fn write_all(&self, path: &Path, contents: &str) -> Result<(), FileSystemError> {
                    self.0
                        .lock()
                        .unwrap()
                        .insert(path.to_path_buf(), contents.as_bytes().to_vec());
                    Ok(())
                }
                fn write_all_bytes(
                    &self,
                    path: &Path,
                    bytes: &[u8],
                ) -> Result<(), FileSystemError> {
                    self.0
                        .lock()
                        .unwrap()
                        .insert(path.to_path_buf(), bytes.to_vec());
                    Ok(())
                }
                fn create_buffered_file_writer(
                    &self,
                    path: &Path,
                    _: Arc<dyn FileDeleter>,
                ) -> Result<Box<dyn BufferedFileWiter>, FileSystemError> {
                    Ok(Box::new(FakeBufferedWriter {
                        path: path.to_path_buf(),
                        disk: Arc::clone(&self.0),
                    }))
                }
                fn exists(&self, path: &Path) -> bool {
                    self.0.lock().unwrap().contains_key(path)
                }
            }

            impl FileReader for FakeDisk {
                fn read_all(&self, path: &Path) -> Result<String, FileSystemError> {
                    let disk = self.0.lock().unwrap();
                    let bytes = disk
                        .get(path)
                        .ok_or(FileSystemError::ExpectedFileError)?
                        .clone();
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                }
                fn read_all_bytes(&self, path: &Path) -> Result<Vec<u8>, FileSystemError> {
                    self.0
                        .lock()
                        .unwrap()
                        .get(path)
                        .cloned()
                        .ok_or(FileSystemError::ExpectedFileError)
                }
                fn exists(&self, path: &Path) -> bool {
                    self.0.lock().unwrap().contains_key(path)
                }
                fn create_reader(
                    &self,
                    path: &Path,
                ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, FileSystemError>
                {
                    let bytes = self
                        .0
                        .lock()
                        .unwrap()
                        .get(path)
                        .cloned()
                        .ok_or(FileSystemError::ExpectedFileError)?;
                    Ok(Box::new(std::io::Cursor::new(bytes)))
                }
            }

            impl FileRenamer for FakeDisk {
                fn rename(&self, from: &Path, to: &Path) -> Result<(), FileSystemError> {
                    let mut disk = self.0.lock().unwrap();
                    if let Some(data) = disk.remove(from) {
                        disk.insert(to.to_path_buf(), data);
                    }
                    Ok(())
                }
            }

            impl FileDeleter for FakeDisk {
                fn delete(&self, path: &Path) -> Result<(), FileSystemError> {
                    self.0.lock().unwrap().remove(path);
                    Ok(())
                }
            }

            fn make_store(disk: FakeDisk) -> FileBlobStore {
                let d = Arc::new(disk);
                FileBlobStore::new(
                    Arc::clone(&d) as Arc<dyn Folder>,
                    Arc::clone(&d) as Arc<dyn FileWriter>,
                    Arc::clone(&d) as Arc<dyn FileReader>,
                    Arc::clone(&d) as Arc<dyn FileRenamer>,
                    Arc::clone(&d) as Arc<dyn FileDeleter>,
                    Arc::clone(&d) as Arc<dyn Inspect>,
                )
            }

            #[tokio::test]
            async fn test_should_round_trip() {
                let sha = "a58dd8680234c1f8cc2ef2b325a43733605a7f16f288e072de8eae81fd8d6433";
                let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.";
                let claim: digest::Digest =
                    format!("sha256:{sha}").parse().expect("valid test digest");
                let blob_root = PathBuf::from("/blobs");
                let store = make_store(FakeDisk::new());
                store
                    .save(&blob_root, &claim, Box::new(data.as_ref()), "mediatype")
                    .await
                    .expect("save should succeed");

                let stats = store
                    .stats(&blob_root, &claim)
                    .await
                    .expect("stats should succeed");
                assert_eq!(stats.mediatype, "mediatype");
                assert_eq!(stats.size, data.len() as u64);

                let mut reader = store
                    .get(&blob_root, &claim)
                    .await
                    .expect("get should succeed");
                let mut actual = Vec::new();
                reader
                    .read_to_end(&mut actual)
                    .await
                    .expect("read should succeed");
                assert_eq!(data.as_ref(), actual);
            }
        }
    }

    mod stats {
        use crate::blob_store::{BlobError, BlobStore, FileBlobStore};
        use crate::digest;
        use file_system::{
            Entry, MockFileDeleter, MockFileReader, MockFileRenamer, MockFileWriter, MockFolder,
            MockInspect,
        };
        use mockall::predicate;
        use std::{path::PathBuf, sync::Arc};

        #[tokio::test]
        async fn test_should_fail_if_no_blob() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let file_reader = MockFileReader::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let blob_root = PathBuf::from("/tmp");
            let mut inspect = MockInspect::new();
            let sha = "ff".repeat(32);

            inspect.given_does_not_exist(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

            let store = FileBlobStore::new(
                Arc::new(folder),
                Arc::new(file_writer),
                Arc::new(file_reader),
                Arc::new(file_renamer),
                Arc::new(file_deleter),
                Arc::new(inspect),
            );

            let request = digest::Digest(format!("sha256:{sha}"));
            let actual = store.stats(&blob_root, &request).await;
            assert!(matches!(
                actual,
                Err(BlobError::DigestNotFound(
                    d
                )) if d ==request.to_string()
            ));
        }

        #[tokio::test]
        async fn test_should_gather_stats() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_reader = MockFileReader::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let blob_root = PathBuf::from("/tmp");
            let mut inspect = MockInspect::new();
            let sha = "ff".repeat(32);

            let path = format!("/tmp/blobs/sha256/ff/{sha}/{sha}");
            inspect.given_exists(&path);
            inspect.expect_entry_with_metadata(&path, Entry::create_file_entry(&sha));
            file_reader.given_can_read_all_with_contents(
                &format!("/tmp/blobs/sha256/ff/{sha}/{sha}.mediatype"),
                "application/vnd.oci.image.manifest.v1+json",
            );

            let store = FileBlobStore::new(
                Arc::new(folder),
                Arc::new(file_writer),
                Arc::new(file_reader),
                Arc::new(file_renamer),
                Arc::new(file_deleter),
                Arc::new(inspect),
            );

            let request = digest::Digest(format!("sha256:{sha}"));
            let actual = store.stats(&blob_root, &request).await;
            assert!(matches!(
                actual,
                Ok(ref s) if s.size == 123 && s.mediatype == "application/vnd.oci.image.manifest.v1+json"
            ));
        }

        #[tokio::test]
        async fn test_should_trim_mediatype_trailing_newline() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_reader = MockFileReader::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let blob_root = PathBuf::from("/tmp");
            let mut inspect = MockInspect::new();
            let sha = "ff".repeat(32);

            let path = format!("/tmp/blobs/sha256/ff/{sha}/{sha}");
            inspect.given_exists(&path);
            inspect.expect_entry_with_metadata(&path, Entry::create_file_entry(&sha));
            file_reader.given_can_read_all_with_contents(
                &format!("/tmp/blobs/sha256/ff/{sha}/{sha}.mediatype"),
                "application/vnd.oci.image.manifest.v1+json\n",
            );

            let store = FileBlobStore::new(
                Arc::new(folder),
                Arc::new(file_writer),
                Arc::new(file_reader),
                Arc::new(file_renamer),
                Arc::new(file_deleter),
                Arc::new(inspect),
            );

            let request = digest::Digest(format!("sha256:{sha}"));
            let actual = store.stats(&blob_root, &request).await;
            assert!(matches!(
                actual,
                Ok(ref s) if s.mediatype == "application/vnd.oci.image.manifest.v1+json"
            ));
        }

        #[tokio::test]
        async fn test_should_fail_if_mediatype_file_missing() {
            let folder = MockFolder::new();
            let file_writer = MockFileWriter::new();
            let mut file_reader = MockFileReader::new();
            let file_renamer = MockFileRenamer::new();
            let file_deleter = MockFileDeleter::new();
            let blob_root = PathBuf::from("/tmp");
            let mut inspect = MockInspect::new();
            let sha = "ff".repeat(32);

            let path = format!("/tmp/blobs/sha256/ff/{sha}/{sha}");
            inspect.given_exists(&path);
            inspect.expect_entry_with_metadata(&path, Entry::create_file_entry(&sha));

            let mediatype_path =
                PathBuf::from(format!("/tmp/blobs/sha256/ff/{sha}/{sha}.mediatype"));
            file_reader
                .expect_read_all()
                .with(predicate::eq(mediatype_path.clone()))
                .returning(move |_| {
                    Err(file_system::FileSystemError::NotFoundError(
                        mediatype_path.clone(),
                    ))
                });

            let store = FileBlobStore::new(
                Arc::new(folder),
                Arc::new(file_writer),
                Arc::new(file_reader),
                Arc::new(file_renamer),
                Arc::new(file_deleter),
                Arc::new(inspect),
            );

            let request = digest::Digest(format!("sha256:{sha}"));
            let actual = store.stats(&blob_root, &request).await;
            assert!(matches!(actual, Err(BlobError::FileSystemError(_))));
        }
    }
}
