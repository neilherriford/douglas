use config::constants::DOUGLAS_ADMIN_GROUP;
use file_system::{Folder, Modes, Permissions, path_to_string};
use log::Logger;
use std::{path::Path, sync::Arc};

pub struct CreateSystemFolder<'a> {
    log: &'a dyn Logger,
    permissions: Arc<dyn Permissions>,
    folder: &'a dyn Folder,
}

impl<'a> CreateSystemFolder<'a> {
    pub fn new(
        log: &'a dyn Logger,
        permissions: Arc<dyn Permissions>,
        folder: &'a dyn Folder,
    ) -> Self {
        Self {
            log,
            permissions,
            folder,
        }
    }

    pub fn perform(&self, description: &str, path: &Path, owning_user: &str, mode: Modes) -> bool {
        if self.folder.exists(path) {
            return true;
        }

        self.log.info(&format! {"Creating {description}…"});
        let pretty_path = path_to_string(path);

        if let Err(err) = self.folder.create_recursively(path) {
            self.log
                .error(&format!("Failed to create path {pretty_path}: {err:?}"));
            return false;
        }

        if let Err(err) =
            self.permissions
                .change_user_and_group_ownership(path, owning_user, DOUGLAS_ADMIN_GROUP)
        {
            self.log.error(&format!(
                "Failed to set permissions on path {pretty_path}: {err:?}"
            ));
            return false;
        }

        self.permissions
            .change_mode(path, &mode)
            .map_err(|err| {
                self.log.error(&format!(
                    "Failed to set mode on path {pretty_path}: {err:?}"
                ));
            })
            .is_ok()
    }
}
