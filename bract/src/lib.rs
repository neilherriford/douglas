pub mod client;
mod encoding;
mod server;
mod version;

use std::path::PathBuf;

pub use client::{Client, ClientError, IoOperation};
use serde::{Deserialize, Serialize};
pub use server::{Server, ServerError};
pub use version::Version;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mount {
    pub name: String,
    pub path: PathBuf,
    pub version: Version,
}
