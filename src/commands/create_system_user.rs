use config::constants::DOUGLAS_GROUP;
use credentials::Credentials;
use log::Logger;

pub struct CreateSystemUser<'a> {
    log: &'a dyn Logger,
    credentials: &'a dyn Credentials,
}

impl<'a> CreateSystemUser<'a> {
    pub fn new(log: &'a dyn Logger, credentials: &'a dyn Credentials) -> Self {
        Self { log, credentials }
    }

    pub fn perform(&self) -> bool {

        self.credentials
            .create_user(RADICLE_USER, RADICLE_GROUP, vec![DOUGLAS_GROUP.to_string()])
            .map_err(|err| {
                self.log
                    .error(&format!("Failed to create user '{RADICLE_USER}': {err:?}"));
            })
            .is_ok()
    }
}
