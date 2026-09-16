use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use thiserror::Error;

const PUBLIC_KEY_HEX: &str = include_str!("../keys/douglas_signing.pub");
const TRAILER_MAGIC: &[u8; 8] = b"SAW_KERF";
const SIGNATURE_LEN: usize = 64;
const TRAILER_LEN: usize = SIGNATURE_LEN + TRAILER_MAGIC.len();

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

fn embedded_verifying_key() -> Result<VerifyingKey, VerifyError> {
    let bytes = hex::decode(PUBLIC_KEY_HEX.trim())
        .map_err(|err| VerifyError::InvalidPublicKey(err.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidPublicKey("expected 32 bytes".to_string()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|err| VerifyError::InvalidPublicKey(err.to_string()))
}

pub(crate) fn verify_binary(path: &Path) -> Result<(), VerifyError> {
    let verifying_key = embedded_verifying_key()?;

    let data =
        std::fs::read(path).map_err(|err| VerifyError::ReadFailed(path.to_path_buf(), err))?;

    if data.len() < TRAILER_LEN {
        return Err(VerifyError::MissingTrailer(path.to_path_buf()));
    }

    let (payload, trailer) = data.split_at(data.len() - TRAILER_LEN);
    let (sig_bytes, magic) = trailer.split_at(SIGNATURE_LEN);

    if magic != TRAILER_MAGIC {
        return Err(VerifyError::MissingTrailer(path.to_path_buf()));
    }

    let Ok(sig_bytes): Result<[u8; SIGNATURE_LEN], _> = sig_bytes.try_into() else {
        return Err(VerifyError::MissingTrailer(path.to_path_buf()));
    };
    let signature = Signature::from_bytes(&sig_bytes);

    verifying_key
        .verify(payload, &signature)
        .map_err(VerifyError::Mismatch)
}
