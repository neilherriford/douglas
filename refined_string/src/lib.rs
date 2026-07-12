use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, marker::PhantomData, str::FromStr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Cannot be empty")]
    CannotBeEmpty,
    #[error("Too long")]
    TooLong,
    #[error("Invalid name '{0}'")]
    InvalidName(String),
}

pub trait StringRules {
    const MAX_LEN: usize;
    fn pattern() -> &'static Regex;
    type Error: From<Error> + std::error::Error;
    fn invalid(value: &str) -> Self::Error;
}

pub struct Validated<R: StringRules> {
    value: String,
    rules: PhantomData<R>,
}

impl<R: StringRules> AsRef<str> for Validated<R> {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

impl<R: StringRules> Clone for Validated<R> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            rules: PhantomData,
        }
    }
}

impl<R: StringRules> std::fmt::Debug for Validated<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Validated").field(&self.value).finish()
    }
}

impl<R: StringRules> PartialEq for Validated<R> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<R: StringRules> Eq for Validated<R> {}

impl<R: StringRules> std::hash::Hash for Validated<R> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<R: StringRules> FromStr for Validated<R> {
    type Err = R::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            Err(Error::CannotBeEmpty.into())
        } else if s.len() > R::MAX_LEN {
            Err(Error::TooLong.into())
        } else if R::pattern().is_match(s) {
            Ok(Self {
                value: s.to_string(),
                rules: PhantomData,
            })
        } else {
            Err(R::invalid(s))
        }
    }
}

impl<R: StringRules> Display for Validated<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

impl<R: StringRules> Serialize for Validated<R> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.value)
    }
}

impl<'de, R: StringRules> Deserialize<'de> for Validated<R> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Validated::<R>::from_str(&value).map_err(serde::de::Error::custom)
    }
}
