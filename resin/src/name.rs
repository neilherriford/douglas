use refined_string::{StringRules, Validated};
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

impl From<refined_string::Error> for NameParseError {
    fn from(err: refined_string::Error) -> Self {
        match err {
            refined_string::Error::CannotBeEmpty => NameParseError::CannotBeEmpty,
            refined_string::Error::TooLong => NameParseError::TooLong,
            refined_string::Error::InvalidName(_) => NameParseError::InvalidName,
        }
    }
}

struct RepositoryComponentRules;

impl StringRules for RepositoryComponentRules {
    const MAX_LEN: usize = 255;

    fn pattern() -> &'static Regex {
        static PATTERN: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[._-][a-z0-9]+)*$").unwrap());
        &PATTERN
    }

    type Error = NameParseError;

    fn invalid(_value: &str) -> Self::Error {
        NameParseError::InvalidName
    }
}

type RepositoryComponent = Validated<RepositoryComponentRules>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Name {
    name: RepositoryComponent,
    namespace: Option<RepositoryComponent>,
}

const ESCAPED_SEPARATOR: &str = "%2F";
const SEPARATOR: &str = "/";

impl Name {
    pub fn from_namespaced(namespace: &str, name: &str) -> Result<Self, NameParseError> {
        let namespace = RepositoryComponent::from_str(namespace)?;
        let name = RepositoryComponent::from_str(name)?;

        if namespace.as_ref().len() + name.as_ref().len() + 1 > 255 {
            Err(NameParseError::TooLong)
        } else {
            Ok(Name {
                name,
                namespace: Some(namespace),
            })
        }
    }

    pub fn fs_safe(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}{ESCAPED_SEPARATOR}{}", self.name),
            None => self.name.to_string(),
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
            None => f.write_str(self.name.as_ref()),
        }
    }
}

impl FromStr for Name {
    type Err = NameParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match Name::split(value) {
            Some((namespace, name)) => Name::from_namespaced(namespace, name),
            None => Ok(Name {
                name: RepositoryComponent::from_str(value)?,
                namespace: None,
            }),
        }
    }
}
