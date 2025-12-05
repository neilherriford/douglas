use config::constants::DOUGLAS_GROUP;
use credentials::Credentials;
use log::Logger;

pub struct CreateSystemGroups<'a> {
    log: &'a dyn Logger,
    credentials: &'a dyn Credentials,
}

impl<'a> CreateSystemGroups<'a> {
    pub fn new(log: &'a dyn Logger, credentials: &'a dyn Credentials) -> Self {
        Self { log, credentials }
    }

    pub fn perform(&self) -> bool {
        self.create_group(DOUGLAS_GROUP)
    }

    fn create_group(&self, name: &str) -> bool {
        if self.credentials.group_exists(name) {
            return true;
        }

        self.log.info(&format!("Creating group '{name}'…"));
        self.credentials
            .create_group(name)
            .map_err(|err| {
                self.log
                    .error(&format!("Failed to create group '{name}': {err:?}"));
            })
            .is_ok()
    }
}
