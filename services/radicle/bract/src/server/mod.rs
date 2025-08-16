use file_system::FileSystemError;
use mockall::automock;

#[macro_use]
mod macros;
mod active_mount_version;
mod create_credentials;
mod create_listener;
mod create_mount;
mod create_new_mount_version;
mod create_system_credentials;
mod list_mount_versions;
mod mount_path_factory;
pub mod server;
mod set_mount_version;
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
