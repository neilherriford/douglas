use log::Logger;

use crate::{
    application_definition::ApplicationDefinition, application_installer::ApplicationInstaller,
};

pub struct BootCoreApplication<'a> {
    log: &'a dyn Logger,
    bract_client: &'a mut bract::Client,
    docker_image_client: &'a mut Box<dyn docker::ImageClient>,
    docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
}

impl<'a> BootCoreApplication<'a> {
    pub fn new(
        log: &'a dyn Logger,
        bract_client: &'a mut bract::Client,
        docker_image_client: &'a mut Box<dyn docker::ImageClient>,
        docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
    ) -> Self {
        Self {
            log,
            bract_client,
            docker_image_client,
            docker_container_client,
        }
    }

    pub async fn perform(&mut self, application_definition: &ApplicationDefinition) -> bool {
        let mut application_installer = ApplicationInstaller::new(
            self.log,
            self.bract_client,
            self.docker_image_client,
            self.docker_container_client,
        );

        let container = match application_installer.install(application_definition).await {
            Ok(container) => container,
            Err(err) => {
                self.log
                    .error(&format!("Could not install OpenBao: {err:?}"));
                return false;
            }
        };

        self.log.info("Starting OpenBao…");
        self.docker_container_client
            .start(&container.id)
            .await
            .map_err(|err| {
                self.log
                    .error(&format!("Could not install OpenBao: {err:?}"));
                err
            })
            .is_ok()
    }
}
