use regex::Regex;
use std::sync::LazyLock;
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NameParseError {
    #[error("Name cannot be empty")]
    CannotBeEmpty,
    #[error("Name too long")]
    TooLong,
    #[error("Name is invalid")]
    InvalidName,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Name {
    name: String,
    namespace: Option<String>,
}

const ESCAPED_SEPARATOR: &str = "%2F";
const SEPARATOR: &str = "/";

impl Name {
    pub fn from_namespaced(namespace: &str, name: &str) -> Result<Self, NameParseError> {
        Name::assert_is_valid(namespace)?;
        Name::assert_is_valid(name)?;

        if namespace.len() + name.len() + 1 > 255 {
            Err(NameParseError::TooLong)
        } else {
            Ok(Name {
                name: name.to_string(),
                namespace: Some(namespace.to_string()),
            })
        }
    }

    pub fn fs_safe(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}{ESCAPED_SEPARATOR}{}", self.name),
            None => self.name.clone(),
        }
    }

    fn assert_is_valid(value: &str) -> Result<(), NameParseError> {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[._-][a-z0-9]+)*$").unwrap());

        if value.is_empty() {
            Err(NameParseError::CannotBeEmpty)
        } else if value.len() > 255 {
            Err(NameParseError::TooLong)
        } else if PATTERN.is_match(value) {
            Ok(())
        } else {
            Err(NameParseError::InvalidName)
        }
    }

    fn split(value: &str) -> Option<(&str, &str)> {
        value
            .split_once(ESCAPED_SEPARATOR)
            .or_else(|| value.split_once(SEPARATOR))
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{namespace}{SEPARATOR}{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

impl FromStr for Name {
    type Err = NameParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match Name::split(value) {
            Some((namespace, name)) => Name::from_namespaced(&namespace, &name),
            None => {
                Name::assert_is_valid(value)?;
                Ok(Name {
                    name: value.to_string(),
                    namespace: None,
                })
            }
        }
    }
}
