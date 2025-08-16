use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::ParseIntError;
use std::ops::Add;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version(pub u8);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl Add<u8> for Version {
    type Output = Version;
    fn add(self, rhs: u8) -> <Self as Add<u8>>::Output {
        Version(self.0 + rhs)
    }
}

#[derive(Error, Debug)]
pub enum VersionParseError {
    #[error("Invalid format")]
    InvalidFormat(String),
    #[error("Invalid number")]
    InvalidNumber(#[from] ParseIntError),
}

impl FromStr for Version {
    type Err = VersionParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut chars = value.chars();

        if let Some(prefix) = chars.next() {
            if prefix == 'v' {
                let version = chars.as_str().parse::<u8>()?;
                Ok(Version(version))
            } else {
                Err(VersionParseError::InvalidFormat(value.to_string()))
            }
        } else {
            Err(VersionParseError::InvalidFormat(value.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    mod from_str {
        use super::super::{Version, VersionParseError};

        #[test]
        fn should_be_non_empty() {
            let actual = "".parse::<Version>();

            assert!(
                matches!(actual, Err(VersionParseError::InvalidFormat(given)) if given == "".to_string())
            );
        }

        #[test]
        fn should_require_v_prefix() {
            let actual = "x123".parse::<Version>();

            assert!(
                matches!(actual, Err(VersionParseError::InvalidFormat(given)) if given == "x123".to_string())
            );
        }

        #[test]
        fn should_be_u8() {
            let actual = "v999".parse::<Version>();

            assert!(matches!(actual, Err(VersionParseError::InvalidNumber(_))));
        }
    }
}
