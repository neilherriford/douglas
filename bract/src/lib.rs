pub mod client;
mod encoding;
mod server;

use std::path::PathBuf;

pub use client::{Client, ClientError, IoOperation};
use serde::{Deserialize, Serialize};
pub use server::{Server, ServerError};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mount {
    pub name: String,
    pub path: PathBuf,
}
