use std::sync::Arc;

use config::{SystemPaths, constants::RADICLE_USER};
use credentials::Credentials;
use file_system::{Folder, Modes, Permissions};
use log::Logger;
use os::EnvironmentVariableReader;

use crate::bail_unless;

use super::{
    AddCurrentUserToSystemGroup, AssertRoot, CreateSystemFolder, CreateSystemGroups,
    CreateSystemUser,
};

pub struct InitializeSystem<'a> {
    system_paths: &'a dyn SystemPaths,
    credentials: &'a dyn Credentials,
    permissions: Arc<dyn Permissions>,
    folder: &'a dyn Folder,
    log: &'a dyn Logger,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
}

impl<'a> InitializeSystem<'a> {
    pub fn new(
        system_paths: &'a dyn SystemPaths,
        credentials: &'a dyn Credentials,
        permissions: Arc<dyn Permissions>,
        folder: &'a dyn Folder,
        log: &'a dyn Logger,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
    ) -> Self {
        Self {
            system_paths,
            credentials,
            permissions,
            folder,
            log,
            environment_variable_reader,
        }
    }

    pub fn perform(&self) -> bool {
        bail_unless!(
            AssertRoot::new(self.log, self.credentials).perform()
                && CreateSystemGroups::new(self.log, self.credentials).perform()
                && CreateSystemUser::new(self.log, self.credentials).perform()
                && AddCurrentUserToSystemGroup::new(
                    self.log,
                    self.credentials,
                    self.environment_variable_reader,
                )
                .perform()
        );

        let create_system_folder =
            CreateSystemFolder::new(self.log, Arc::clone(&self.permissions), self.folder);

        bail_unless!(
            create_system_folder.perform(
                "log root",
                &self.system_paths.log_root(),
                credentials::ROOT_USER_NAME,
                Modes::OwnerReadWriteGroupReadWrite,
            ) && create_system_folder.perform(
                "runtime root",
                &self.system_paths.runtime_root(),
                credentials::ROOT_USER_NAME,
                Modes::OwnerReadWriteGroupRead,
            ) && create_system_folder.perform(
                "service root",
                &self.system_paths.service_root(),
                RADICLE_USER,
                Modes::OwnerReadWriteGroupRead,
            ) && create_system_folder.perform(
                "mount root",
                &self.system_paths.mount_root(),
                RADICLE_USER,
                Modes::OwnerReadWriteGroupRead,
            )
        );

        true
    }
}
