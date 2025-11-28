use log::Logger;

use super::{AssertDockerIsRunning, CreateNetwork};

pub struct InitializeDocker<'a> {
    log: &'a dyn Logger,
    system_client: &'a mut dyn docker::SystemClient,
    network_client: &'a mut dyn docker::NetworkClient,
}

impl<'a> InitializeDocker<'a> {
    pub fn new(
        log: &'a dyn Logger,
        system_client: &'a mut dyn docker::SystemClient,
        network_client: &'a mut dyn docker::NetworkClient,
    ) -> Self {
        Self {
            log,
            system_client,
            network_client,
        }
    }
    pub async fn perform(&mut self) -> bool {
        AssertDockerIsRunning::new(self.log, self.system_client)
            .perform()
            .await
            && CreateNetwork::new(self.log, self.network_client)
                .perform("douglas")
                .await
    }
}
