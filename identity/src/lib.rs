use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{
        Aead, Generate, KeyInit,
        array::Array,
        consts::{U12, U32},
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use config::DouglasFolders;
use file_system::{FileReader, FileSystemError, FileWriter};
use hkdf::Hkdf;
use sha2::Sha256;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("cipher text was not valid base64")]
    InvalidEncoding,
    #[error("decrypted bytes were not valid utf-8")]
    InvalidUtf8,
    #[error("invalid identity")]
    InvalidIdentity,
    #[error("could not derive key")]
    KeyError,
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Identity: Send + Sync {
    fn initialize(&mut self) -> Result<(), Error>;
    fn encrypt(&mut self, intent: &Intent, plain_text: String) -> Result<String, Error>;
    fn decrypt(&mut self, intent: &Intent, cipher_text: String) -> Result<String, Error>;
}

fn identity_file_path() -> PathBuf {
    let mut path = DouglasFolders::new().identity;
    path.push("identity");
    path
}

fn create_identity() -> [u8; 32] {
    Array::<u8, U32>::generate().into()
}

pub struct LocalIdentity {
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    cached_identity: Option<[u8; 32]>,
}

pub enum Intent {
    Unseal,
    Authentication,
}

impl Intent {
    fn label(&self) -> &'static [u8] {
        match self {
            Intent::Unseal => b"UNSEAL_KEY_PURPOSE",
            Intent::Authentication => b"AUTHENTICATION",
        }
    }
}

impl LocalIdentity {
    pub fn new(file_reader: Arc<dyn FileReader>, file_writer: Arc<dyn FileWriter>) -> Self {
        Self {
            file_reader,
            file_writer,
            cached_identity: None,
        }
    }

    fn identity(&mut self) -> Result<[u8; 32], Error> {
        if let Some(identity) = self.cached_identity {
            return Ok(identity);
        }

        let read = self.file_reader.read_all_bytes(&identity_file_path())?;
        let result: [u8; 32] = read.try_into().map_err(|_| Error::InvalidIdentity)?;
        self.cached_identity = Some(result);

        Ok(result)
    }

    fn derive_key(&mut self, intent: &Intent) -> Result<Key<Aes256Gcm>, Error> {
        let hk = Hkdf::<Sha256>::new(None, &self.identity()?);
        let mut okm = [0u8; 32];
        hk.expand(intent.label(), &mut okm)
            .map_err(|_| Error::KeyError)?;
        Ok(Key::<Aes256Gcm>::from(okm))
    }
}

impl Identity for LocalIdentity {
    fn initialize(&mut self) -> Result<(), Error> {
        let identity = create_identity();

        if self
            .file_writer
            .write_all_bytes_if_absent(&identity_file_path(), &identity)?
        {
            self.cached_identity = Some(identity);
        }

        Ok(())
    }

    fn encrypt(&mut self, intent: &Intent, plain_text: String) -> Result<String, Error> {
        let cipher = Aes256Gcm::new(&self.derive_key(intent)?);
        let nonce = Nonce::<U12>::generate();

        let cipher_bytes = cipher
            .encrypt(&nonce, plain_text.as_bytes())
            .map_err(|_| Error::Encrypt)?;

        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&cipher_bytes);
        Ok(STANDARD.encode(combined))
    }

    fn decrypt(&mut self, intent: &Intent, cipher_text: String) -> Result<String, Error> {
        let cipher = Aes256Gcm::new(&self.derive_key(intent)?);

        let combined = STANDARD
            .decode(cipher_text)
            .map_err(|_| Error::InvalidEncoding)?;
        let (nonce_bytes, cipher_bytes) = combined
            .split_at_checked(12)
            .ok_or(Error::InvalidEncoding)?;

        let nonce = Nonce::<U12>::try_from(nonce_bytes).map_err(|_| Error::InvalidEncoding)?;

        let plain_bytes = cipher
            .decrypt(&nonce, cipher_bytes)
            .map_err(|_| Error::Decrypt)?;

        String::from_utf8(plain_bytes).map_err(|_| Error::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileReader, MockFileWriter};

    const STORED_IDENTITY: [u8; 32] = [7u8; 32];

    fn identity_with_stored_bytes(bytes: [u8; 32]) -> LocalIdentity {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all_bytes()
            .returning(move |_| Ok(bytes.to_vec()));

        LocalIdentity::new(Arc::new(file_reader), Arc::new(MockFileWriter::new()))
    }

    #[test]
    fn test_encrypt_then_decrypt_should_round_trip() {
        let mut identity = identity_with_stored_bytes(STORED_IDENTITY);

        let cipher_text = identity
            .encrypt(&Intent::Unseal, "top secret".to_string())
            .expect("should encrypt");
        let plain_text = identity
            .decrypt(&Intent::Unseal, cipher_text)
            .expect("should decrypt");

        assert_eq!(plain_text, "top secret");
    }

    #[test]
    fn test_decrypt_should_fail_when_the_intent_does_not_match_encryption() {
        let mut identity = identity_with_stored_bytes(STORED_IDENTITY);

        let cipher_text = identity
            .encrypt(&Intent::Unseal, "top secret".to_string())
            .expect("should encrypt");
        let result = identity.decrypt(&Intent::Authentication, cipher_text);

        assert!(matches!(result, Err(Error::Decrypt)));
    }

    #[test]
    fn test_decrypt_should_fail_when_cipher_text_is_not_valid_base64() {
        let mut identity = identity_with_stored_bytes(STORED_IDENTITY);

        let result = identity.decrypt(&Intent::Unseal, "not-valid-base64!!!".to_string());

        assert!(matches!(result, Err(Error::InvalidEncoding)));
    }

    #[test]
    fn test_decrypt_should_fail_when_cipher_text_is_too_short_to_contain_a_nonce() {
        let mut identity = identity_with_stored_bytes(STORED_IDENTITY);
        let too_short = STANDARD.encode(b"short");

        let result = identity.decrypt(&Intent::Unseal, too_short);

        assert!(matches!(result, Err(Error::InvalidEncoding)));
    }

    #[test]
    fn test_encrypt_should_fail_when_the_stored_identity_is_not_32_bytes() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all_bytes()
            .returning(|_| Ok(vec![1, 2, 3]));

        let mut identity =
            LocalIdentity::new(Arc::new(file_reader), Arc::new(MockFileWriter::new()));

        let result = identity.encrypt(&Intent::Unseal, "top secret".to_string());

        assert!(matches!(result, Err(Error::InvalidIdentity)));
    }

    #[test]
    fn test_identity_should_only_be_read_from_disk_once() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all_bytes()
            .times(1)
            .returning(|_| Ok(STORED_IDENTITY.to_vec()));

        let mut identity =
            LocalIdentity::new(Arc::new(file_reader), Arc::new(MockFileWriter::new()));

        identity
            .encrypt(&Intent::Unseal, "first".to_string())
            .expect("should encrypt");
        identity
            .encrypt(&Intent::Unseal, "second".to_string())
            .expect("should encrypt");
    }

    #[test]
    fn test_initialize_should_cache_a_freshly_written_identity() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all_bytes_if_absent()
            .returning(|_, _| Ok(true));

        let mut identity =
            LocalIdentity::new(Arc::new(MockFileReader::new()), Arc::new(file_writer));

        identity.initialize().expect("should initialize");

        identity
            .encrypt(&Intent::Unseal, "top secret".to_string())
            .expect("should encrypt using the cached identity");
    }

    #[test]
    fn test_initialize_should_not_error_when_an_identity_already_exists() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all_bytes_if_absent()
            .returning(|_, _| Ok(false));

        let mut identity =
            LocalIdentity::new(Arc::new(MockFileReader::new()), Arc::new(file_writer));

        assert!(identity.initialize().is_ok());
    }
}
