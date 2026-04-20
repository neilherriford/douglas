use crate::{
    encoding::safe_file_system_name,
    server::{
        Credential, MountDefinition, Shared,
        service_account_manager::{self, ServiceAccountManager, ServiceCredentials},
    },
};
use config::{SystemPaths, constants::DOUGLAS_ADMIN_GROUP};
use credentials::Credentials;
use file_system::{FileSystemError, Folder, Modes, Permissions, path_to_string};
use postcard::{from_bytes, to_allocvec};
use serde::{Deserialize, Serialize};
use sled::{Db, Tree};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug)]
struct ServiceEntry {
    path: PathBuf,
    mounts: Vec<MountEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MountEntry {
    name: String,
    path: PathBuf,
    shared_with: Option<Credential>,
    ephemeral: bool,
}

#[derive(Error, Debug)]
pub struct Mount {
    pub name: String,
    pub shared: Shared,
    pub ephemeral: bool,
}

impl std::fmt::Display for Mount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = match &self.shared {
            Shared::No => "not shared".to_string(),
            Shared::WithServices(service_names) => format!(
                "shared with {}",
                service_names
                    .iter()
                    .map(|name| format!("'{name}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };

        let ephemeral = if self.ephemeral {
            "ephemeral"
        } else {
            "persistent"
        };

        f.write_str(&format!("Mount: {}, {ephemeral}, {shared}", self.name))
    }
}

#[derive(Error, Debug)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

impl std::fmt::Display for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mounts_str = self
            .mounts
            .iter()
            .map(|mount| mount.to_string())
            .collect::<Vec<_>>()
            .join(", ");

        f.write_str(&format!("Service: {}, mounts [{}]", self.name, mounts_str))
    }
}

#[derive(Error, Debug)]
pub enum ServiceMountManagerError {
    #[error("IO error: {0}")]
    IoError(#[from] sled::Error),
    #[error("Serialization error: {0}")]
    SerializationError(#[from] postcard::Error),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] credentials::CredentialsError),
    #[error("File system error: {0}")]
    FileSystemError(#[from] file_system::FileSystemError),
    #[error("Service Credentials error: {0}")]
    ServiceCredentialsError(#[from] service_account_manager::ServiceAccountManagerError),
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
}

impl ServiceMountManagerError {
    pub fn to_client_string(self) -> String {
        match self {
            ServiceMountManagerError::IoError(e) => format!("IO error: {}", e),
            ServiceMountManagerError::SerializationError(e) => {
                format!("Serialization error: {}", e)
            }
            ServiceMountManagerError::CredentialsError(e) => format!("Credentials error: {}", e),
            ServiceMountManagerError::FileSystemError(e) => format!("File system error: {}", e),
            ServiceMountManagerError::ServiceCredentialsError(e) => {
                format!("Service credentials error: {}", e)
            }
            ServiceMountManagerError::ConfigurationError(msg) => msg.clone(),
        }
    }
}

pub trait ServiceMountManager {
    fn get_or_create(
        &self,
        service_name: &str,
        mounts: &HashSet<MountDefinition>,
    ) -> Result<Service, ServiceMountManagerError>;
    fn list(&self) -> Result<Vec<Service>, FileSystemError>;
}

pub struct LocalServiceMountManager {
    services: Tree,
    service_account_manager: Box<dyn ServiceAccountManager>,
    folder: Arc<dyn Folder>,
    credentials: Arc<dyn Credentials>,
    permissions: Arc<dyn Permissions>,
    system_paths: Arc<dyn SystemPaths>,
}

impl LocalServiceMountManager {
    pub fn build(
        bract_data: &Db,
        rolodex: Box<dyn ServiceAccountManager>,
        system_paths: Arc<dyn SystemPaths>,
        folder: Arc<dyn Folder>,
        credentials: Arc<dyn Credentials>,
        permissions: Arc<dyn Permissions>,
    ) -> Result<Self, sled::Error> {
        let services = bract_data.open_tree("service_mounts")?;

        Ok(Self {
            services,
            service_account_manager: rolodex,
            folder,
            credentials,
            permissions,
            system_paths,
        })
    }

    fn set_ownership(
        &self,
        path: &Path,
        user_name: &str,
        group_name: &str,
        mode: Modes,
    ) -> Result<(), FileSystemError> {
        self.permissions
            .change_user_and_group_ownership(path, user_name, group_name)?;
        self.permissions.change_mode(path, &mode)
    }

    fn initialize_mount_root(&self) -> Result<(), ServiceMountManagerError> {
        let mount_root = self.system_paths.mount_root();
        let mount_root = mount_root.as_path();

        if !self.folder.exists(mount_root) {
            self.folder.create_recursively(mount_root)?;
            self.set_ownership(
                mount_root,
                credentials::ROOT_USER_NAME,
                DOUGLAS_ADMIN_GROUP,
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            )?;
        }

        Ok(())
    }

    fn create_path(
        &self,
        path: &PathBuf,
        user_name: &str,
        group_name: &str,
        inherit_group: bool,
    ) -> Result<bool, ServiceMountManagerError> {
        let path = path.as_path();
        let mode = if inherit_group {
            Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute
        } else {
            Modes::OwnerReadWriteExecuteGroupReadWriteExecute
        };

        if self.folder.exists(path) {
            self.verify_path(user_name, group_name, path, mode)?;
            Ok(false)
        } else {
            self.folder.create_recursively(path)?;
            self.set_ownership(path, user_name, group_name, mode)?;
            Ok(true)
        }
    }

    fn verify_path(
        &self,
        user_name: &str,
        group_name: &str,
        path: &Path,
        mode: Modes,
    ) -> Result<(), ServiceMountManagerError> {
        let (actual_user_name, actual_group_name) =
            self.permissions.get_user_and_group_ownership(path)?;
        if actual_group_name != group_name {
            return Err(ServiceMountManagerError::ConfigurationError(format!(
                "Path {} exits, and was expcted to be owned by group '{group_name}' and user '{user_name}' but is instead owned by group '{actual_group_name}'",
                path_to_string(path)
            )));
        }
        if actual_user_name != user_name {
            return Err(ServiceMountManagerError::ConfigurationError(format!(
                "Path {} exits, and was expcted to be owned by group '{group_name}' and user '{user_name}' but is instead owned by user '{actual_user_name}'",
                path_to_string(path)
            )));
        }
        let actual_mode = self.permissions.get_mode(path)?;
        if actual_mode != mode {
            return Err(ServiceMountManagerError::ConfigurationError(format!(
                "Path {} exits, and was expcted to have permissions {mode} but was {actual_mode}",
                path_to_string(path)
            )));
        }
        Ok(())
    }

    fn build_service_root_path(&self, service_name: &str) -> PathBuf {
        let mut service_root = self.system_paths.mount_root();
        service_root.push(safe_file_system_name(service_name));
        service_root
    }

    fn build_mount_root_path(&self, service_name: &str, mount_name: &str) -> PathBuf {
        let mut mount_root = self.build_service_root_path(service_name);
        mount_root.push(safe_file_system_name(mount_name));
        mount_root
    }

    fn create_from_definition(
        &self,
        service_name: &str,
        mounts: &HashSet<MountDefinition>,
    ) -> Result<ServiceEntry, ServiceMountManagerError> {
        todo!();
        // self.initialize_mount_root()?;

        // let service_account = self.create_service_account(service_name, &mounts)?;

        // let service_root_path = self.build_service_root_path(service_name);
        // self.create_path(
        //     &service_root_path,
        //     &service_account.user.system_name,
        //     &service_account.group.system_name,
        //     false,
        // )?;

        // let mut mount_entries = Vec::<MountEntry>::new();

        // for mount in mounts {
        //     let group_name;
        //     let shared;
        //     if let Some(share_group) = service_account.shares.get(&mount.name) {
        //         group_name = share_group.system_name.clone();
        //         shared = true;
        //     } else {
        //         group_name = service_account.group.system_name.clone();
        //         shared = false;
        //     }

        //     let mount_path = self.build_mount_root_path(service_name, &mount.name);
        //     self.create_path(
        //         &mount_path,
        //         &service_account.user.system_name,
        //         &group_name,
        //         shared,
        //     )?;

        //     mount_entries.push(MountEntry {
        //         name: mount.name.clone(),
        //         path: mount_path,
        //         ephemeral: mount.ephemeral,
        //         shared_with: service_account.shares.get(&mount.name).cloned(),
        //     })
        // }

        // Ok(ServiceEntry {
        //     path: service_root_path,
        //     mounts: mount_entries,
        // })
    }

    // fn create_service_account(
    //     &self,
    //     service_name: &str,
    //     mounts: &HashSet<MountDefinition>,
    // ) -> Result<service_account_manager::ServiceAccount, ServiceMountManagerError> {
    //     let shares = HashSet::from_iter(mounts.iter().filter_map(|mount| match &mount.shared {
    //         crate::server::Shared::No => None,
    //         crate::server::Shared::WithServices(services) => Some(service_account_manager::Share {
    //             name: mount.name.clone(),
    //             guest_service_names: services.clone().into_iter().collect(),
    //         }),
    //     }));
    //     let service_account = self
    //         .service_account_manager
    //         .get_or_create_service_account(service_name, &shares)?;
    //     Ok(service_account)
    // }

    // fn verify_definition(
    //     &self,
    //     service_name: &str,
    //     service_entry: &ServiceEntry,
    // ) -> Result<(), ServiceMountManagerError> {
    //     let actual_service_account = self
    //         .service_account_manager
    //         .get_service_account(service_name)?;

    //     if !self.folder.exists(&service_entry.path) {
    //         return Err(ServiceMountManagerError::ConfigurationError(format!(
    //             "Missing service directory: {}",
    //             path_to_string(service_entry.path)
    //         )));
    //     }
    //     self.verify_path(
    //         &actual_service_account.user.system_name,
    //         &actual_service_account.group.system_name,
    //         &service_entry.path,
    //         Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
    //     )?;

    //     for mount in service_entry.mounts {
    //         if !self.folder.exists(&mount.path) {
    //             return Err(ServiceMountManagerError::ConfigurationError(format!(
    //                 "Service {service_name} mount {} missing service directory: {}",
    //                 mount.name,
    //                 path_to_string(&mount.path)
    //             )));
    //         }

    //         let group_name;
    //         let mode;
    //         if let Some(share_group) = actual_service_account.shares.get(&mount.name) {
    //             group_name = share_group.system_name.clone();
    //             mode = Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute
    //         } else {
    //             group_name = actual_service_account.group.system_name.clone();
    //             mode = Modes::OwnerReadWriteExecuteGroupReadWriteExecute;
    //         }

    //         self.verify_path(
    //             &actual_service_account.user.system_name,
    //             &group_name,
    //             &mount.path,
    //             mode,
    //         )?;
    //     }
    //     Ok(())
    // }
}

impl ServiceMountManager for LocalServiceMountManager {
    fn get_or_create(
        &self,
        service_name: &str,
        mounts: &HashSet<MountDefinition>,
    ) -> Result<Service, ServiceMountManagerError> {
        // let service_entry = if let Some(bytes) = self.services.get(service_name)? {
        //     let service_entry: ServiceEntry = from_bytes(&bytes)?;
        //     self.verify_definition(service_name, &service_entry)?;
        //     service_entry
        // } else {
        //     let service_entry = self.create_from_definition(service_name, &mounts)?;
        //     let bytes = to_allocvec(&service_entry)?;
        //     self.services.insert(service_name, bytes);
        //     service_entry
        // };

        todo!()
        // return Ok(Service {
        //     name: service_name.to_string(),
        //     mounts: service_entry.mounts.iter().map(|mount_entry| Mount {
        //         name: mount_entry.name.clone(),
        //         shared: match mount_entry.shared_with {
        //             Some(share_credential) =>

        //             self.credentials.group_memberships(share_credential.name)?.iter().filter_map(|user_name|

        //             ),
        //             None => Shared::No,
        //         },
        //         ephemeral: mount_entry.ephemeral
        //     } )
        // });
    }

    fn list(&self) -> Result<Vec<Service>, FileSystemError> {
        todo!()
    }
}
