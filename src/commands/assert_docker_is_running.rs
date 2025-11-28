use docker::PingResult;
use log::Logger;

pub struct AssertDockerIsRunning<'a> {
    log: &'a dyn Logger,
    client: &'a mut dyn docker::SystemClient,
}

impl<'a> AssertDockerIsRunning<'a> {
    pub fn new(log: &'a dyn Logger, client: &'a mut dyn docker::SystemClient) -> Self {
        Self { log, client }
    }

    pub async fn perform(&mut self) -> bool {
        self.log.info("Verifying Docker is running…");

        match self.client.ping().await {
            Ok(PingResult::Ok) => true,
            Ok(PingResult::Error(message)) => {
                self.log
                    .error(&format!("Docker is not running: '{message}'"));
                false
            }
            Err(err) => {
                self.log.error(&format!(
                    "Could not verify if Docker is not running: '{err}'"
                ));
                false
            }
        }
    }
}
