use bract::client::Credential;
use docker::{ContainerDefinition, ContainerUser, DockerError};
use file_system::path_to_string;
use log::Logger;
use std::{collections::HashMap, path::PathBuf};
use thiserror::Error;

use crate::{
    application_definition::ApplicationDefinition,
    mount_file_template_expander::MountFileTemplateExpander,
};

#[derive(Error, Debug)]
pub enum ApplicationInstallerError {
    #[error("Bract client error: {0}")]
    BractClient(#[from] bract::ClientError),

    #[error("Docker client error: {0}")]
    DockerClient(#[from] docker::DockerError),

    #[error("Missing named mount: {0}")]
    MissingNamedMount(String),
}

pub struct ApplicationInstaller<'a> {
    bract_client: &'a bract::Client,
    docker_image_client: &'a mut Box<dyn docker::ImageClient>,
    docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
    log: &'a dyn Logger,
}

impl<'a> ApplicationInstaller<'a> {
    pub fn new(
        log: &'a dyn Logger,
        bract_client: &'a bract::Client,
        docker_image_client: &'a mut Box<dyn docker::ImageClient>,
        docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
    ) -> Self {
        Self {
            bract_client,
            docker_image_client,
            docker_container_client,
            log,
        }
    }

    pub async fn install(
        &mut self,
        definition: &ApplicationDefinition,
    ) -> Result<docker::Container, ApplicationInstallerError> {
        self.log.info(&format!("Installing {}…", definition.name));
        let credential = self.create_credentials(definition).await?;
        let image = self.install_image(definition).await?;

        let mount_name_to_path = self.create_mount_folders(definition).await?;
        let mut template_expander = MountFileTemplateExpander::new();
        template_expander
            .with_runas_credentail(&credential)
            .with_douglas_group();

        self.inject_files(definition, &template_expander).await?;

        let result = self
            .create_container(definition, credential, mount_name_to_path, image)
            .await?;

        Ok(result)
    }

    async fn create_credentials(
        &mut self,
        definition: &ApplicationDefinition,
    ) -> Result<Credential, ApplicationInstallerError> {
        self.log
            .info(&format!("Creating credentials for {}…", definition.name));

        let result = self
            .bract_client
            .create_credentials(&definition.name)
            .await?;

        Ok(result)
    }

    async fn install_image(
        &mut self,
        definition: &ApplicationDefinition,
    ) -> Result<docker::Image, ApplicationInstallerError> {
        self.log
            .info(&format!("Installing Docker image for {}…", definition.name,));

        match self
            .docker_image_client
            .find_by_name(&definition.image_name)
            .await
        {
            Ok(image) => Ok(image),
            Err(DockerError::NotFoundError) => Ok(self
                .docker_image_client
                .pull(&definition.image_name)
                .await?),
            Err(err) => Err(err.into()),
        }
    }

    async fn create_mount_folders(
        &self,
        definition: &ApplicationDefinition,
    ) -> Result<HashMap<String, PathBuf>, ApplicationInstallerError> {
        self.log
            .info(&format!("Creating mount folders for {}…", definition.name));

        let mut result = HashMap::<String, PathBuf>::new();

        for mount_template in &definition.mount_templates {
            self.log.info(&format!(
                "Creating mount storage for {}/{}…",
                definition.name, mount_template.name
            ));
            let mount = match self
                .bract_client
                .active_mount_version(&definition.name, &mount_template.name)
                .await
            {
                Ok(Some(mount)) => mount,
                Ok(None) => {
                    self.bract_client
                        .create_mount(
                            &definition.name,
                            &mount_template.name,
                            mount_template.shared,
                        )
                        .await?
                }
                Err(err) => return Err(err.into()),
            };

            result.insert(mount_template.name.clone(), mount.path);
        }

        Ok(result)
    }

    async fn inject_files(
        &self,
        definition: &ApplicationDefinition,
        template_expander: &MountFileTemplateExpander,
    ) -> Result<(), ApplicationInstallerError> {
        self.log
            .info(&format!("Adding mount files for {}…", definition.name));

        for (mount_name, file) in definition
            .mount_templates
            .iter()
            .flat_map(|template| template.files.iter().map(|file| (&template.name, file)))
        {
            self.log.info(&format!(
                "Writing {}:{}:{}…",
                definition.name,
                mount_name,
                path_to_string(&file.relative_path),
            ));

            let contents = template_expander.expand(&file.contents);

            self.bract_client
                .mount_io(
                    &definition.name,
                    mount_name,
                    bract::IoOperation::WriteFile {
                        relative_path: file.relative_path.as_path(),
                        contents: &contents,
                    },
                )
                .await?;
        }

        Ok(())
    }

    async fn create_container(
        &mut self,
        definition: &ApplicationDefinition,
        credential: Credential,
        mount_name_to_host_path: HashMap<String, PathBuf>,
        image: docker::Image,
    ) -> Result<docker::Container, ApplicationInstallerError> {
        self.log
            .info(&format!("Installing Docker container {}…", definition.name));

        match self
            .docker_container_client
            .find_by_name(&definition.name)
            .await
        {
            Ok(container) => Ok(container),
            Err(DockerError::NotFoundError) => {
                self.log
                    .info(&format!("Creating Docker container {}…", definition.name));
                let container_definition = ContainerDefinition {
                    name: definition.name.clone(),
                    run_as: Some(ContainerUser {
                        user_id: credential.user_id,
                        group_id: credential.group_id,
                    }),
                    command: definition.command.clone(),
                    environment_variables: definition.environment_variables.clone(),
                    image_name: docker::ImageIdentifier::Id(image.id),
                    mounts: Self::create_docker_mount_definitions(
                        definition,
                        &mount_name_to_host_path,
                    )?,
                    added_capabilities: definition.added_capabilities.clone(),
                    labels: definition.labels.clone(),
                };

                let container = self
                    .docker_container_client
                    .create(container_definition)
                    .await?;
                Ok(container)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn create_docker_mount_definitions(
        defintion: &ApplicationDefinition,
        mount_name_to_host_path: &HashMap<String, PathBuf>,
    ) -> Result<Vec<docker::MountDefinition>, ApplicationInstallerError> {
        let mut result = Vec::<docker::MountDefinition>::new();

        for mount_definintion in &defintion.mount_templates {
            if let Some(host_path) = mount_name_to_host_path.get(&mount_definintion.name) {
                result.push(docker::MountDefinition {
                    name: mount_definintion.name.clone(),
                    host_path: host_path.clone(),
                    container_path: mount_definintion.container_path.clone(),
                    writable: mount_definintion.writable,
                });
            } else {
                return Err(ApplicationInstallerError::MissingNamedMount(
                    defintion.name.clone(),
                ));
            }
        }

        Ok(result)
    }
}
