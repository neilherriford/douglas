use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DigestError {
    #[error("Invalid digest")]
    InvalidDigest,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Digest(pub String);

impl Digest {
    pub fn from_bytes<T: AsRef<[u8]>>(value: &T) -> Result<Self, DigestError> {
        if value.as_ref().len() != 32 {
            return Err(DigestError::InvalidDigest);
        }
        let hex = hex::encode(value);
        Ok(Digest(format!("sha256:{hex}")))
    }

    pub fn from_hex(hex: &str) -> Result<Self, DigestError> {
        let result: Digest = format!("sha256:{hex}").parse()?;
        Ok(result)
    }

    pub fn hex(&self) -> &str {
        &self.0["sha256:".len()..]
    }
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut result = Vec::new();
        let mut chars = self.hex().chars();
        while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
            let hi = hi.to_digit(16).unwrap() as u8;
            let lo = lo.to_digit(16).unwrap() as u8;
            result.push((hi << 4) | lo);
        }
        result
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Digest {
    type Err = DigestError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix("sha256:")
            .ok_or(DigestError::InvalidDigest)?;
        if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Digest(value.to_owned()))
        } else {
            Err(DigestError::InvalidDigest)
        }
    }
}
