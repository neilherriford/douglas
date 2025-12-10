use docker::DockerError;
use log::Logger;

pub struct CreateNetwork<'a> {
    log: &'a dyn Logger,
    client: &'a mut dyn docker::NetworkClient,
}

impl<'a> CreateNetwork<'a> {
    pub fn new(log: &'a dyn Logger, client: &'a mut dyn docker::NetworkClient) -> Self {
        Self { log, client }
    }

    pub async fn perform(&mut self, name: &str) -> bool {
        match self.client.inspect_by_name("douglas").await {
            Ok(_) => true,
            Err(DockerError::ResourceNotFound) => {
                self.log.info("Creating Douglas core Docker network");
                self.client
                    .create(name, Vec::new())
                    .await
                    .map_err(|err| {
                        self.log
                            .error(&format!("Could not create douglas docker network: {err:?}"));
                    })
                    .is_ok()
            }
            Err(err) => {
                self.log
                    .error(&format!("Could not check douglas docker network: {err:?}"));
                false
            }
        }
    }
}
