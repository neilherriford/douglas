use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml::value::Datetime;

#[derive(Error, Debug)]
pub enum ServiceLibraryError {
    #[error("oops")]
    Oops,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Mount {
    pub name: String,
    pub created_at: Datetime,
    pub shared_with_services: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub version: usize,
    pub active: bool,
    pub mounts: Vec<Mount>,
    pub created_at: Datetime,
}

pub trait ServiceLibrary {
    fn list(&self) -> Vec<Service>;
    fn create(&self, name: &str, mounts: &[Mount]) -> Result<Service, ServiceLibraryError>;
    fn load(&self, name: &str) -> Result<Service, ServiceLibraryError>;
    fn deactivate(&self, name: &str) -> Result<(), ServiceLibraryError>;
    fn activate(&self, name: &str) -> Result<(), ServiceLibraryError>;
}
