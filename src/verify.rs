use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use thiserror::Error;

const PUBLIC_KEY_HEX: &str = include_str!("../keys/douglas_signing.pub");
const TRAILER_MAGIC: &[u8; 8] = b"SAW_KERF";
const VERSION_LEN: usize = 3;
const SIGNATURE_LEN: usize = 64;
const TRAILER_LEN: usize = VERSION_LEN + SIGNATURE_LEN + TRAILER_MAGIC.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

#[derive(Error, Debug)]
pub(crate) enum VerifyError {
    #[error("embedded public key is invalid: {0}")]
    InvalidPublicKey(String),
    #[error("failed to read {0}: {1}")]
    ReadFailed(PathBuf, std::io::Error),
    #[error("{0} is not a signed douglas binary")]
    MissingTrailer(PathBuf),
    #[error("signature does not match: {0}")]
    Mismatch(ed25519_dalek::SignatureError),
}

#[derive(Error, Debug)]
enum TrailerError {
    #[error("missing or malformed signature trailer")]
    MissingTrailer,
    #[error("signature does not match: {0}")]
    Mismatch(ed25519_dalek::SignatureError),
}

fn embedded_verifying_key() -> Result<VerifyingKey, VerifyError> {
    let bytes = hex::decode(PUBLIC_KEY_HEX.trim())
        .map_err(|err| VerifyError::InvalidPublicKey(err.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidPublicKey("expected 32 bytes".to_string()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|err| VerifyError::InvalidPublicKey(err.to_string()))
}

pub(crate) fn verify_binary(path: &Path) -> Result<Version, VerifyError> {
    let verifying_key = embedded_verifying_key()?;

    let data =
        std::fs::read(path).map_err(|err| VerifyError::ReadFailed(path.to_path_buf(), err))?;

    verify_trailer(&verifying_key, &data).map_err(|err| match err {
        TrailerError::MissingTrailer => VerifyError::MissingTrailer(path.to_path_buf()),
        TrailerError::Mismatch(err) => VerifyError::Mismatch(err),
    })
}

fn verify_trailer(verifying_key: &VerifyingKey, data: &[u8]) -> Result<Version, TrailerError> {
    if data.len() < TRAILER_LEN {
        return Err(TrailerError::MissingTrailer);
    }

    let (payload, trailer) = data.split_at(data.len() - TRAILER_LEN);
    let (version_bytes, rest) = trailer.split_at(VERSION_LEN);
    let (sig_bytes, magic) = rest.split_at(SIGNATURE_LEN);

    if magic != TRAILER_MAGIC {
        return Err(TrailerError::MissingTrailer);
    }

    let Ok(sig_bytes): Result<[u8; SIGNATURE_LEN], _> = sig_bytes.try_into() else {
        return Err(TrailerError::MissingTrailer);
    };
    let signature = Signature::from_bytes(&sig_bytes);

    let mut signed_message = payload.to_vec();
    signed_message.extend_from_slice(version_bytes);

    verifying_key
        .verify(&signed_message, &signature)
        .map_err(TrailerError::Mismatch)?;

    Ok(Version {
        major: version_bytes[0],
        minor: version_bytes[1],
        patch: version_bytes[2],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_keypair() -> (SigningKey, VerifyingKey) {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
    }

    fn signed_data(signing_key: &SigningKey, payload: &[u8], version: [u8; 3]) -> Vec<u8> {
        let mut message = payload.to_vec();
        message.extend_from_slice(&version);

        let signature = signing_key.sign(&message);

        let mut data = payload.to_vec();
        data.extend_from_slice(&version);
        data.extend_from_slice(&signature.to_bytes());
        data.extend_from_slice(TRAILER_MAGIC);
        data
    }

    #[test]
    fn test_verify_trailer_should_accept_a_correctly_signed_payload_and_version() {
        let (signing_key, verifying_key) = test_keypair();
        let data = signed_data(&signing_key, b"pretend binary bytes", [1, 2, 3]);

        let Ok(version) = verify_trailer(&verifying_key, &data) else {
            panic!("should verify");
        };

        assert_eq!(
            version,
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
        );
    }

    #[test]
    fn test_verify_trailer_should_reject_a_tampered_payload() {
        let (signing_key, verifying_key) = test_keypair();
        let mut data = signed_data(&signing_key, b"pretend binary bytes", [1, 2, 3]);
        data[0] ^= 0xFF;

        let result = verify_trailer(&verifying_key, &data);

        assert!(matches!(result, Err(TrailerError::Mismatch(_))));
    }

    #[test]
    fn test_verify_trailer_should_reject_a_tampered_version_even_though_the_signature_bytes_are_untouched()
     {
        let (signing_key, verifying_key) = test_keypair();
        let mut data = signed_data(&signing_key, b"pretend binary bytes", [1, 2, 3]);
        let version_start = data.len() - TRAILER_LEN;
        data[version_start] = 99;

        let result = verify_trailer(&verifying_key, &data);

        assert!(matches!(result, Err(TrailerError::Mismatch(_))));
    }

    #[test]
    fn test_verify_trailer_should_reject_a_missing_magic() {
        let (signing_key, verifying_key) = test_keypair();
        let mut data = signed_data(&signing_key, b"pretend binary bytes", [1, 2, 3]);
        let last = data.len() - 1;
        data[last] ^= 0xFF;

        let result = verify_trailer(&verifying_key, &data);

        assert!(matches!(result, Err(TrailerError::MissingTrailer)));
    }

    #[test]
    fn test_verify_trailer_should_reject_a_binary_with_no_trailer_at_all() {
        let (_, verifying_key) = test_keypair();
        let data = b"just a plain, unsigned binary".to_vec();

        let result = verify_trailer(&verifying_key, &data);

        assert!(matches!(result, Err(TrailerError::MissingTrailer)));
    }

    #[test]
    fn test_verify_trailer_should_reject_a_signature_from_a_different_key() {
        let (_, verifying_key) = test_keypair();
        let other_signing_key = SigningKey::from_bytes(&[9u8; 32]);
        let data = signed_data(&other_signing_key, b"pretend binary bytes", [1, 2, 3]);

        let result = verify_trailer(&verifying_key, &data);

        assert!(matches!(result, Err(TrailerError::Mismatch(_))));
    }
}
