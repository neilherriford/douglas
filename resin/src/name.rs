use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NameParseError {
    #[error("Name cannot be empty")]
    CannotBeEmpty,
}

pub enum Name {
    Simple(String),
    Namespaced(String, String),
}

const ESCAPED_SEPARATOR: &str = "%2F";
const SEPARATOR: &str = "/";

impl Name {
    pub fn fs_safe(&self) -> String {
        match self {
            Name::Simple(name) => name.clone(),
            Name::Namespaced(namespace, name) => format!("{namespace}{ESCAPED_SEPARATOR}{name}"),
        }
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Name::Simple(name) => f.write_str(name),
            Name::Namespaced(namespace, name) => write!(f, "{namespace}{SEPARATOR}{name}"),
        }
    }
}

impl FromStr for Name {
    type Err = NameParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();

        if value.is_empty() {
            return Err(NameParseError::CannotBeEmpty);
        }

        match value.split_once(ESCAPED_SEPARATOR) {
            Some((namespace, name)) => {
                let namespace = namespace.trim();
                let name = name.trim();
                if namespace.is_empty() || name.is_empty() {
                    Err(NameParseError::CannotBeEmpty)
                } else {
                    Ok(Name::Namespaced(namespace.to_string(), name.to_string()))
                }
            }
            None => Ok(Name::Simple(value.to_string())),
        }
    }
}
