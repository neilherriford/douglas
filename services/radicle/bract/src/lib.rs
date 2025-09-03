pub mod client;
mod encoding;
mod server;
mod version;

pub use client::Client;
pub use server::server::{Server, ServerError};
pub use version::Version;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum Request {
    ActiveMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
    },
    CreateCredentials {
        token: String,
        service_name: String,
    },
    CreateMount {
        token: String,
        service_name: String,
        mount_name: String,
    },
    CreateNewMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
    },
    ListMountVersions {
        token: String,
        service_name: String,
        mount_name: String,
    },
    SetMountVersion {
        token: String,
        service_name: String,
        mount_name: String,
        version: Version,
    },
    Status {
        token: String,
    },
    Shutdown {
        token: String,
    },
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Request::ActiveMountVersion {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "ActiveMountVersion service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::CreateCredentials {
                token: _,
                service_name,
            } => format!("CreateCredentials service_name: '{}'", service_name),
            Request::CreateMount {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "CreateMount service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::CreateNewMountVersion {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "CreateNewMountVersion service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::ListMountVersions {
                token: _,
                service_name,
                mount_name,
            } => format!(
                "ListMountVersions service_name: '{}', mount_name: '{}'",
                service_name, mount_name
            ),
            Request::SetMountVersion {
                token: _,
                service_name,
                mount_name,
                version,
            } => format!(
                "SetMountVersion service_name: '{}', mount_name: '{}', version: '{}'",
                service_name, mount_name, version
            ),
            Request::Status { token: _ } => "Status".to_string(),
            Request::Shutdown { token: _ } => "Shutdown".to_string(),
        };

        write!(f, "{}", value)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mount {
    pub name: String,
    pub active: Version,
    pub available: Vec<Version>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "data")]
pub enum Response {
    CredentialsCreated {
        user: String,
        group: String,
    },
    MountSet {
        version: Version,
        path: PathBuf,
    },
    MountVersionsListed(Vec<Version>),
    InvalidToken,
    Status {
        token_path: PathBuf,
        mount_root: PathBuf,
        services: Vec<Service>,
    },
    Error(String),
    ShuttingDown,
    Stopped,
}
