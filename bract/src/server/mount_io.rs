use crate::encoding::safe_prefixed_credential_name;
use file_system::{FileSystemError, FileWriter, Folder, Modes, Permissions, path_to_string};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use utils::ClientErrorDisplay;

pub struct MountIo {
    // mount_path_factory: Arc<MountPathFactory>,
    folder: Arc<dyn Folder>,
    file_writer: Arc<dyn FileWriter>,
    permissions: Arc<dyn Permissions>,
}

#[derive(Error, Debug)]
pub enum MountIoError {
    #[error("Mount '{mount_name}' does not exist for serivce '{service_name}'.")]
    MountDoesNotExist {
        service_name: String,
        mount_name: String,
    },
    #[error("Path must be relative {0}")]
    NotRelativePath(PathBuf),

    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
    // #[error("Mount path version error {0}")]
    // MountPathVersionError(#[from] MountPathVersionError),
}

impl ClientErrorDisplay for MountIoError {
    fn to_client_string(&self) -> String {
        match self {
            MountIoError::MountDoesNotExist {
                service_name,
                mount_name,
            } => format!(
                "Could not write into '{}' mount of service '{}', has it been created yet?",
                mount_name, service_name
            ),
            MountIoError::NotRelativePath(path_buf) => format!(
                "Expected a relative path, but receieved an absolute one: '{}'",
                path_to_string(path_buf)
            ),
            _ => "Failed to write into mount".to_string(),
        }
    }
}

impl MountIo {
    pub fn new(
        folder: Arc<dyn Folder>,
        file_writer: Arc<dyn FileWriter>,
        permissions: Arc<dyn Permissions>,
    ) -> Self {
        todo!();
        // Self {
        //     mount_path_factory,
        //     folder,
        //     file_writer,
        //     permissions,
        // }
    }

    pub fn write_file(
        &self,
        service_name: &str,
        mount_name: &str,
        relative_path: PathBuf,
        contents: &str,
    ) -> Result<(), MountIoError> {
        let active_version_path = self.assert_active_mount_exists(service_name, mount_name)?;

        if !relative_path.is_relative() {
            return Err(MountIoError::NotRelativePath(relative_path));
        }

        if let Some(parent) = self.folder.parent(relative_path.as_path()) {
            self.create_path(active_version_path.clone(), parent, service_name)?;
        }

        let file_path = self
            .folder
            .combine(active_version_path.as_path(), relative_path.as_path());
        let file_path = file_path.as_path();
        self.file_writer.write_all(file_path, contents)?;
        self.set_ownership(service_name, file_path)?;

        Ok(())
    }

    fn assert_active_mount_exists(
        &self,
        service_name: &str,
        mount_name: &str,
    ) -> Result<PathBuf, MountIoError> {
        todo!();
        // let active_version_path = self
        //     .mount_path_factory
        //     .active_version_path(service_name, mount_name);

        // if self.folder.exists(active_version_path.as_path()) {
        //     return Ok(active_version_path);
        // }
        // Err(MountIoError::MountDoesNotExist {
        //     service_name: service_name.to_string(),
        //     mount_name: mount_name.to_string(),
        // })
    }

    fn create_path(
        &self,
        active_version_path: PathBuf,
        relative_path: PathBuf,
        service_name: &str,
    ) -> Result<(), MountIoError> {
        let mut working = active_version_path.to_path_buf();
        for component in self.folder.split(relative_path.as_path()) {
            working = self.folder.combine(working.as_path(), component.as_path());
            let working_path = working.as_path();
            if self.folder.exists(working_path) {
                continue;
            }
            self.folder.create_recursively(working_path)?;
            self.set_ownership(service_name, working_path)?;
        }
        Ok(())
    }

    fn set_ownership(&self, service_name: &str, path: &Path) -> Result<(), FileSystemError> {
        todo!()
        // let (user_name, group_name) = safe_prefixed_credential_name(service_name);
        // self.permissions
        //     .change_user_and_group_ownership(path, &user_name, &group_name)?;
        // self.permissions
        //     .change_mode(path, &Modes::OwnerReadWriteGroupReadWrite)
    }
}
