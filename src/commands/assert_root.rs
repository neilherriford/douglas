use credentials::Credentials;
use log::Logger;

pub struct AssertRoot<'a> {
    log: &'a dyn Logger,
    credentials: &'a dyn Credentials,
}

impl<'a> AssertRoot<'a> {
    pub fn new(log: &'a dyn Logger, credentials: &'a dyn Credentials) -> Self {
        Self { log, credentials }
    }

    pub fn perform(&self) -> bool {
        if self.credentials.is_root() {
            return true;
        }
        self.log.error("Douglas must be run as root!");
        false
    }
}
