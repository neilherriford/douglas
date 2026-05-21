use bract::client::Credential;
use docker::{ContainerDefinition, ContainerUser, DockerError};
use file_system::path_to_string;
use log::{Level, Reporter, ScopeKind, Span};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
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
    reporter: Arc<dyn Reporter>,
    bract_client: &'a bract::Client,
    docker_image_client: &'a mut Box<dyn docker::ImageClient>,
    docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
}

impl<'a> ApplicationInstaller<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        bract_client: &'a bract::Client,
        docker_image_client: &'a mut Box<dyn docker::ImageClient>,
        docker_container_client: &'a mut Box<dyn docker::ContainerClient>,
    ) -> Self {
        Self {
            reporter,
            bract_client,
            docker_image_client,
            docker_container_client,
        }
    }

    pub async fn install(
        &mut self,
        definition: &dyn ApplicationDefinition,
    ) -> Result<docker::Container, ApplicationInstallerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Installer: Install application",
            ScopeKind::Task,
        )
        .start_guard();

        guard
            .span()
            .message(Level::Info, &format!("Installing {}…", definition.name()));
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

        guard.finish(Ok(result))
    }

    async fn create_credentials(
        &mut self,
        definition: &dyn ApplicationDefinition,
    ) -> Result<Credential, ApplicationInstallerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Installer: Create credentials",
            ScopeKind::Task,
        )
        .start_guard();

        guard.span().message(
            Level::Info,
            &format!("Creating credentials for {}…", definition.name()),
        );

        let result = self
            .bract_client
            .create_credentials(&definition.name())
            .await?;

        guard.finish(Ok(result))
    }

    async fn install_image(
        &mut self,
        definition: &dyn ApplicationDefinition,
    ) -> Result<docker::Image, ApplicationInstallerError> {
        let guard =
            Span::new(Arc::clone(&self.reporter), "Install image", ScopeKind::Task).start_guard();

        guard.span().message(
            Level::Info,
            &format!("Installing Docker image for {}…", definition.name(),),
        );

        guard.finish(
            match self
                .docker_image_client
                .find_by_name(&definition.image_name())
                .await
            {
                Ok(image) => Ok(image),
                Err(DockerError::ResourceNotFound) => Ok(self
                    .docker_image_client
                    .pull(&definition.image_name())
                    .await?),
                Err(err) => Err(err.into()),
            },
        )
    }

    async fn create_mount_folders(
        &self,
        definition: &dyn ApplicationDefinition,
    ) -> Result<HashMap<String, PathBuf>, ApplicationInstallerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Installer: Create mount folders",
            ScopeKind::Task,
        )
        .start_guard();

        guard.span().message(
            Level::Info,
            &format!("Creating mount folders for {}…", definition.name()),
        );

        let mut result = HashMap::<String, PathBuf>::new();

        for mount_template in &definition.mount_templates() {
            guard.span().message(
                Level::Info,
                &format!(
                    "Creating mount storage for {}/{}…",
                    definition.name(),
                    mount_template.name
                ),
            );
            let mount = match self
                .bract_client
                .active_mount_version(&definition.name(), &mount_template.name)
                .await
            {
                Ok(Some(mount)) => mount,
                Ok(None) => {
                    self.bract_client
                        .create_mount(
                            &definition.name(),
                            &mount_template.name,
                            mount_template.shared,
                        )
                        .await?
                }
                Err(err) => return Err(err.into()),
            };

            result.insert(mount_template.name.clone(), mount.path);
        }

        guard.finish(Ok(result))
    }

    async fn inject_files(
        &self,
        definition: &dyn ApplicationDefinition,
        template_expander: &MountFileTemplateExpander,
    ) -> Result<(), ApplicationInstallerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Installer: Inject files",
            ScopeKind::Task,
        )
        .start_guard();

        guard.span().message(
            Level::Info,
            &format!("Adding mount files for {}…", definition.name()),
        );

        for (mount_name, file) in definition
            .mount_templates()
            .iter()
            .flat_map(|template| template.files.iter().map(|file| (&template.name, file)))
        {
            guard.span().message(
                Level::Info,
                &format!(
                    "Writing {}:{}:{}…",
                    definition.name(),
                    mount_name,
                    path_to_string(&file.relative_path),
                ),
            );

            let contents = template_expander.expand(file);

            self.bract_client
                .mount_io(
                    &definition.name(),
                    mount_name,
                    bract::IoOperation::WriteFile {
                        relative_path: file.relative_path.as_path(),
                        contents: &contents,
                    },
                )
                .await?;
        }

        guard.finish(Ok(()))
    }

    async fn create_container(
        &mut self,
        definition: &dyn ApplicationDefinition,
        credential: Credential,
        mount_name_to_host_path: HashMap<String, PathBuf>,
        image: docker::Image,
    ) -> Result<docker::Container, ApplicationInstallerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Installer create container",
            log::ScopeKind::Task,
        )
        .start_guard();
        guard.span().message(
            Level::Info,
            &format!("Installing Docker container {}…", definition.name()),
        );

        match self
            .docker_container_client
            .find_by_name(&definition.name())
            .await
        {
            Ok(container) => Ok(container),
            Err(DockerError::ResourceNotFound) => {
                guard.span().message(
                    Level::Info,
                    &format!("Creating Docker container {}…", definition.name()),
                );
                let container_definition = ContainerDefinition {
                    name: definition.name(),
                    run_as: Some(ContainerUser {
                        user_id: credential.user_id,
                        group_id: credential.group_id,
                    }),
                    command: definition.command(),
                    environment_variables: definition.environment_variables(),
                    image_name: docker::ImageIdentifier::Id(image.id),
                    mounts: Self::create_docker_mount_definitions(
                        definition,
                        &mount_name_to_host_path,
                    )?,
                    added_capabilities: definition.added_capabilities(),
                    labels: definition.labels(),
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
        definition: &dyn ApplicationDefinition,
        mount_name_to_host_path: &HashMap<String, PathBuf>,
    ) -> Result<Vec<docker::MountDefinition>, ApplicationInstallerError> {
        let mut result = Vec::<docker::MountDefinition>::new();

        for mount_definintion in &definition.mount_templates() {
            if let Some(host_path) = mount_name_to_host_path.get(&mount_definintion.name) {
                result.push(docker::MountDefinition {
                    name: mount_definintion.name.clone(),
                    host_path: host_path.clone(),
                    container_path: mount_definintion.container_path.clone(),
                    writable: mount_definintion.writable,
                });
            } else {
                return Err(ApplicationInstallerError::MissingNamedMount(
                    definition.name(),
                ));
            }
        }

        Ok(result)
    }
}
