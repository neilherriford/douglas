use std::env::VarError;

use config::constants::RADICLE_GROUP;
use credentials::Credentials;
use log::Logger;
use os::EnvironmentVariableReader;

pub struct AddCurrentUserToSystemGroup<'a> {
    log: &'a dyn Logger,
    credentials: &'a dyn Credentials,
    environment_variable_reader: &'a dyn EnvironmentVariableReader,
}

impl<'a> AddCurrentUserToSystemGroup<'a> {
    pub fn new(
        log: &'a dyn Logger,
        credentials: &'a dyn Credentials,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
    ) -> Self {
        Self {
            log,
            credentials,
            environment_variable_reader,
        }
    }

    pub fn perform(&self) -> bool {
        match self.environment_variable_reader.read("SUDO_USER") {
            Ok(user_name) => {
                if user_name == credentials::ROOT_USER_NAME {
                    self.log.warn(&format!(
                        "The sudo user is already root, not adding to {RADICLE_GROUP}!"
                    ));
                    return true;
                }
                self.add_to_system_group(&user_name)
            }
            Err(VarError::NotPresent) => true,
            Err(VarError::NotUnicode(_)) => {
                self.log.warn(&format!(
                        "Could not determine initiating user?  You will need to manually add the \
                            account you wish to interact with the Douglas CLI to the '{RADICLE_GROUP}' \
                            manually!"
                    ));
                true
            }
        }
    }

    fn add_to_system_group(&self, user_name: &str) -> bool {
        let groups = self.credentials.group_memberships(user_name);
        if groups.contains(&RADICLE_GROUP.to_string()) {
            return true;
        }

        self.log
            .info(&format!("Adding {user_name} to {RADICLE_GROUP}…"));

        self.credentials
            .join_group(user_name, RADICLE_GROUP)
            .map_err(|err| {
                self.log.error(&format!(
                    "Failed to add user {user_name} to the {RADICLE_GROUP} group: {err:?}"
                ));
            })
            .is_ok()
    }
}
