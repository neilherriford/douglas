use crate::digest;
use file_system::{FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder};
use sha2::Sha256;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug, Error)]
pub enum BlobError {
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

pub trait BlobStore {
    async fn save(
        &self,
        claimed: &digest::Digest,
        source: impl AsyncRead + Send + Unpin,
    ) -> Result<(), BlobError>;

    async fn get(
        &self,
        digest: &digest::Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, BlobError>;

    async fn exists(&self, digest: &digest::Digest) -> Result<bool, BlobError>;
}

struct DigestPaths {
    pub root: PathBuf,
    pub temp_file: PathBuf,
    pub final_file: PathBuf,
}

impl DigestPaths {
    pub fn new(blob_root: &PathBuf, digest: &digest::Digest) -> Self {
        let sha = digest.hex().to_string();
        let prefix = sha[0..2].to_string();

        let mut root = blob_root.clone();
        root.push("sha256");
        root.push(prefix);
        root.push(&sha);
        let mut temp_file = root.clone();
        temp_file.push(format!("{sha}.tmp"));
        let mut final_file = root.clone();
        final_file.push(sha);

        Self {
            root,
            temp_file,
            final_file,
        }
    }
}

struct DigestStore {
    folder: Arc<dyn Folder>,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    initialized: bool,
    completed: bool,
    digest_paths: DigestPaths,
}

impl DigestStore {
    pub fn new(
        blob_root: PathBuf,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
        folder: Arc<dyn Folder>,
        digest: &digest::Digest,
    ) -> Self {
        Self {
            folder,
            file_renamer,
            file_deleter,
            initialized: false,
            completed: false,
            digest_paths: DigestPaths::new(&blob_root, digest),
        }
    }

    pub fn initialize(&mut self) -> Result<PathBuf, FileSystemError> {
        self.folder.create_recursively(&self.digest_paths.root)?;
        self.initialized = true;
        Ok(self.digest_paths.temp_file.clone())
    }

    pub fn complete(&mut self) -> Result<(), FileSystemError> {
        match self
            .file_renamer
            .rename(&self.digest_paths.temp_file, &self.digest_paths.final_file)
        {
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
        self.file_deleter.delete(&self.digest_paths.temp_file)?;
        self.file_deleter.delete(&self.digest_paths.root)?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for DigestStore {
    fn drop(&mut self) {
        if !self.initialized || self.completed {
            return;
        }

        let _ = self.clean_up();
    }
}

pub struct FileBlobStore {
    folder: Arc<dyn Folder>,
    file_writer: Arc<dyn FileWriter>,
    file_reader: Arc<dyn FileReader>,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    blob_root: PathBuf,
}

impl FileBlobStore {
    pub fn new(
        folder: Arc<dyn Folder>,
        file_writer: Arc<dyn FileWriter>,
        file_reader: Arc<dyn FileReader>,
        file_renamer: Arc<dyn FileRenamer>,
        file_deleter: Arc<dyn FileDeleter>,
        blob_root: PathBuf,
    ) -> Self {
        Self {
            folder,
            file_writer,
            file_reader,
            file_renamer,
            file_deleter,
            blob_root,
        }
    }

    async fn hashed_read_to_temp_file(
        &self,
        mut source: impl AsyncRead + Send + Unpin,
        temp_file: &PathBuf,
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
        let computed = sha2::Digest::finalize(hasher);

        Ok(computed)
    }
}

impl BlobStore for FileBlobStore {
    async fn save(
        &self,
        claimed: &digest::Digest,
        source: impl AsyncRead + Send + Unpin,
    ) -> Result<(), BlobError> {
        if self.exists(claimed).await? {
            return Ok(());
        }

        let mut digest_store = DigestStore::new(
            self.blob_root.clone(),
            Arc::clone(&self.file_renamer),
            Arc::clone(&self.file_deleter),
            Arc::clone(&self.folder),
            claimed,
        );

        let temp_path = digest_store.initialize()?;
        let hash = self.hashed_read_to_temp_file(source, &temp_path).await?;
        let computed = hash.as_slice();

        if claimed.as_bytes().as_slice() != computed {
            digest_store.clean_up()?;

            return Err(BlobError::DigestMismatch {
                claimed: claimed.clone(),
                computed: digest::Digest::from_bytes(&computed)?,
            });
        }

        digest_store.complete()?;
        Ok(())
    }

    async fn exists(&self, digest: &digest::Digest) -> Result<bool, BlobError> {
        let digest_paths = DigestPaths::new(&self.blob_root, digest);
        Ok(self.folder.exists(&digest_paths.final_file))
    }

    async fn get(
        &self,
        digest: &digest::Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, BlobError> {
        let digest_paths = DigestPaths::new(&self.blob_root, digest);
        let result = self
            .file_reader
            .create_reader(&digest_paths.final_file)
            .await?;
        Ok(result)
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
                MockFileRenamer, MockFileWriter, MockFolder,
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
                let blob_root = PathBuf::from("/tmp/blobs");

                folder.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
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
                    blob_root,
                );

                let source = std::io::Cursor::new(Vec::new());
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store.save(&digest, source).await;
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
                let blob_root = PathBuf::from("/tmp/blobs");

                folder.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer
                    .expect_write_all()
                    .returning(|_| Err(FileSystemError::ExpectedFileError));

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store.save(&digest, source).await;

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
                let blob_root = PathBuf::from("/tmp/blobs");

                folder.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");

                let buffered_writer = MockBufferedFileWiter::new();

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                let source = FailingReader {};
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store.save(&digest, source).await;

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
                let blob_root = PathBuf::from("/tmp/blobs");

                folder.given_does_not_exist("/tmp/blobs/sha256/f0/f00d/f00d");
                folder.expect_create_folder_recursively_with("/tmp/blobs/sha256/f0/f00d");
                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d/f00d.tmp");
                file_deleter.expect_file_to_be_deleted("/tmp/blobs/sha256/f0/f00d");

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest("sha256:f00d".to_string());
                let result = store.save(&digest, source).await;

                assert!(matches!(result, Err(BlobError::DigestMismatch { .. })));
            }

            #[tokio::test]
            async fn test_should_fail_if_rename_failed() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let mut file_renamer = MockFileRenamer::new();
                let mut file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                folder.given_does_not_exist(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"
                ));
                folder.expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}"
                ));

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));

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
                file_deleter
                    .expect_file_to_be_deleted(&format!("/tmp/blobs/sha256/05/{actual_sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store.save(&digest, source).await;

                assert!(matches!(
                    result,
                    Err(BlobError::FileSystemError(
                        FileSystemError::ExpectedFileError
                    ))
                ));
            }

            #[tokio::test]
            async fn test_should_not_overwrite_writ() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                folder.given_exists(&format!("/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}/"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store.save(&digest, source).await;

                assert!(matches!(result, Ok(())))
            }

            #[tokio::test]
            async fn test_should_write_blob() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let mut file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let actual_sha = "05174bbf0d407087e45b12baae17117426852ff3a9e58d12a0ebb9a10b409743";
                folder.given_does_not_exist(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}/{actual_sha}"
                ));
                folder.expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/05/{actual_sha}"
                ));

                let mut buffered_writer = MockBufferedFileWiter::new();

                buffered_writer.expect_write_all().returning(|_| Ok(()));

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
                    blob_root,
                );

                let source = std::io::Cursor::new(vec![0xBA, 0xAD, 0xF0, 0x0D]);
                let digest = digest::Digest(format!("sha256:{actual_sha}"));
                let result = store.save(&digest, source).await;

                assert!(matches!(result, Ok(())))
            }
        }

        mod exists {
            use crate::blob_store::{BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                MockFileDeleter, MockFileReader, MockFileRenamer, MockFileWriter, MockFolder,
            };
            use std::{path::PathBuf, sync::Arc};

            #[tokio::test]
            async fn test_should_determine_if_blob_does_not_exist() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let sha = "ff".repeat(32);
                folder.given_does_not_exist(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );
                let actual = store.exists(&digest::Digest(format!("sha256:{sha}"))).await;
                assert!(matches!(actual, Ok(false)));
            }

            #[tokio::test]
            async fn test_should_determine_if_blob_does_exist() {
                let mut folder = MockFolder::new();
                let file_writer = MockFileWriter::new();
                let file_reader = MockFileReader::new();
                let file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let sha = "ff".repeat(32);
                folder.given_exists(&format!("/tmp/blobs/sha256/ff/{sha}/{sha}"));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );
                let actual = store.exists(&digest::Digest(format!("sha256:{sha}"))).await;
                assert!(matches!(actual, Ok(true)));
            }
        }

        mod get {
            use crate::blob_store::{BlobError, BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                FileSystemError, MockFileDeleter, MockFileReader, MockFileRenamer, MockFileWriter,
                MockFolder,
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
                let blob_root = PathBuf::from("/tmp/blobs");

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
                    blob_root,
                );
                let actual = store.get(&digest::Digest(format!("sha256:{sha}"))).await;
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
                let blob_root = PathBuf::from("/tmp/blobs");

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
                    blob_root,
                );
                let actual = store.get(&digest::Digest(format!("sha256:{sha}"))).await;
                assert!(actual.is_ok());
            }
        }

        mod round_trip {
            use crate::blob_store::{BlobStore, FileBlobStore};
            use crate::digest;
            use file_system::{
                MockBufferedFileWiter, MockFileDeleter, MockFileReader, MockFileRenamer,
                MockFileWriter, MockFolder,
            };
            use mockall::predicate;
            use std::{path::PathBuf, sync::Arc};
            use tokio::io::AsyncReadExt;

            #[tokio::test]
            async fn test_should_roundtrip() {
                let mut folder = MockFolder::new();
                let mut file_writer = MockFileWriter::new();
                let mut file_reader = MockFileReader::new();
                let mut file_renamer = MockFileRenamer::new();
                let file_deleter = MockFileDeleter::new();
                let blob_root = PathBuf::from("/tmp/blobs");

                let sha = "a58dd8680234c1f8cc2ef2b325a43733605a7f16f288e072de8eae81fd8d6433";
                let data = "Lorem ipsum dolor sit amet, consectetur adipiscing elit.".as_bytes();

                let prefixed_sha = format!("sha256:{sha}");
                let claim: digest::Digest = prefixed_sha
                    .parse()
                    .expect("test fixture digest should be valid");

                let prefix = sha[0..2].to_string();
                let expected_final_file =
                    PathBuf::from(format!("/tmp/blobs/sha256/{prefix}/{sha}/{sha}"));
                let expected_temp_file =
                    PathBuf::from(format!("/tmp/blobs/sha256/{prefix}/{sha}/{sha}.tmp"));

                folder.given_does_not_exist(&format!("/tmp/blobs/sha256/{prefix}/{sha}/{sha}"));
                folder.expect_create_folder_recursively_with(&format!(
                    "/tmp/blobs/sha256/{prefix}/{sha}"
                ));

                let mut buffered_writer = MockBufferedFileWiter::new();
                buffered_writer.expect_write_all().returning(|_| Ok(()));

                file_writer
                    .expect_create_buffered_file_writer()
                    .return_once(move |_, _| Ok(Box::new(buffered_writer)));

                file_renamer
                    .expect_rename()
                    .with(
                        predicate::eq(expected_temp_file.clone()),
                        predicate::eq(expected_final_file.clone()),
                    )
                    .returning(|_, _| Ok(()));

                file_reader
                    .expect_create_reader()
                    .with(predicate::eq(expected_final_file.clone()))
                    .returning(move |_| Ok(Box::new(std::io::Cursor::new(data))));

                let store = FileBlobStore::new(
                    Arc::new(folder),
                    Arc::new(file_writer),
                    Arc::new(file_reader),
                    Arc::new(file_renamer),
                    Arc::new(file_deleter),
                    blob_root,
                );

                store.save(&claim, data).await.expect("should save");
                let mut reader = store.get(&claim).await.expect("should retrieve");

                let mut actual = Vec::new();
                reader.read_to_end(&mut actual).await.expect("should read");
                assert_eq!(data, actual);
            }
        }
    }
}
