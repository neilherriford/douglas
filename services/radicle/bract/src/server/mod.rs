use crate::{Service, Version};
use file_system::FileSystemError;
use mockall::automock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[macro_use]
mod macros;
mod active_mount_version;
mod create_credentials;
mod create_listener;
mod create_mount;
mod create_new_mount_version;
mod list_mount_versions;
mod mount_path_factory;
pub mod server;
mod set_mount_version;
mod shutdown;
mod status;
mod token_refresher;
mod token_validator;
mod version_manager;

#[automock]
pub(super) trait ClientErrorDisplay {
    fn to_client_string(&self) -> String;
}

impl ClientErrorDisplay for FileSystemError {
    fn to_client_string(&self) -> String {
        "Could not create mount".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "data")]
pub(crate) enum Response {
    CredentialsCreated {
        user: String,
        group: String,
    },
    MountSet {
        name: String,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub(crate) enum Request {
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
