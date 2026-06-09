use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ApplicationDefinitionRepositryError {
    #[error("Ping failed: {0}")]
    PingFailed(String),
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub enum ShareAccess {
    Readonly,
    ReadWrite,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ShareDefinition {
    pub group_name: String,
    pub access: ShareAccess,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct MountDefinition {
    pub name: String,
    pub container_path: PathBuf,
    pub shared_with: Vec<ShareDefinition>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SupportFile {
    name: String,
    description: String,
    relative_path: PathBuf,
    contents: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplicationDefinition {
    alias: String,
    name: String,
    version: String,
    namespace: String,
    dependencies: Vec<String>,
    environment_variables: HashMap<String, String>,
    support_files: Vec<SupportFile>,
    mounts: Vec<MountDefinition>,
}

pub trait ApplicationDefinitionRepositry {
    fn list(&self) -> Result<Vec<ApplicationDefinition>, ApplicationDefinitionRepositryError>;
}

pub struct FileApplicationDefinitionRepositry {
    root: PathBuf,
}

impl FileApplicationDefinitionRepositry {
    pub fn new(root: PathBuf) -> Self {
        Self { root: root.clone() }
    }
}
