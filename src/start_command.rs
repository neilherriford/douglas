use colored::Colorize;
use config::{SystemPaths, create_system_paths};
use credentials::create_credentials;
use file_system::{
    FileAppender, FileReader, LocalFileAppender, LocalFileReader, LocalFolder, LocalPermissions,
    Permissions,
};
use log::Logger;
use openbao::{OpenBaoClient, SimpleOpenBaoClient};
use os::{Os, Unix, UnixEnvironmentVariableReader};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

use crate::{
    commands, core_applications, deferred_file_logger::DeferredFileLogger, file_logger::FileLogger,
    tee_logger::TeeLogger,
};

pub struct StartCommand {
    log: TeeLogger,
    system_paths: Box<dyn SystemPaths>,
    file_appender: Arc<dyn FileAppender>,
    permissions: Arc<dyn Permissions>,
}

impl StartCommand {
    pub fn new() -> Self {
        let system_paths = create_system_paths();
        let log_path = system_paths.log_path("douglas");

        let file_appender: Arc<dyn FileAppender> = Arc::new(LocalFileAppender::new());
        let permissions: Arc<dyn Permissions> = Arc::new(LocalPermissions::new());

        Self {
            system_paths,
            file_appender: Arc::clone(&file_appender),
            log: TeeLogger::new(Box::new(DeferredFileLogger::new(&log_path, file_appender))),
            permissions,
        }
    }

    pub async fn perform(&self) -> bool {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = LocalFolder::new();
        let environment_variable_reader = UnixEnvironmentVariableReader::new();

        bail_unless!(
            commands::InitializeSystem::new(
                self.system_paths.as_ref(),
                credentials.as_ref(),
                Arc::clone(&self.permissions),
                &folder,
                &self.log,
                &environment_variable_reader,
            )
            .perform()
        );
        bail_unless!(self.initialize_docker().await);

        let mut bract_client = self.create_bract_client();
        bail_unless!(
            commands::StartBract::new(&self.log, &mut bract_client, Arc::clone(&os))
                .perform()
                .await
        );

        let docker_logger: Arc<dyn Logger> = Arc::new(FileLogger::new(
            &self.system_paths.log_path("docker"),
            Arc::clone(&self.file_appender),
        ));

        let mut docker_image_client = unwrap_or_bail!(
            self.create_docker_image_client(Arc::clone(&docker_logger))
                .await
        );
        let mut docker_container_client =
            unwrap_or_bail!(self.create_docker_container_client(docker_logger).await);

        let openbao_definition = core_applications::OpenBao::new();
        bail_unless!(
            commands::BootCoreApplication::new(
                &self.log,
                &mut bract_client,
                &mut docker_image_client,
                &mut docker_container_client,
            )
            .perform(&openbao_definition)
            .await
        );

        self.log.info("Initializing OpenBao");

        let openbao_logger: Arc<dyn Logger> = Arc::new(FileLogger::new(
            self.system_paths.log_path("openbao").as_path(),
            Arc::clone(&self.file_appender),
        ));

        let socket_path = match openbao_definition.qualified_socket_path(bract_client).await {
            Ok(socket_path) => socket_path,
            Err(err) => {
                self.log.error(&format!(
                    "Could not determine qualified socket path for OpenBao: {err}"
                ));
                return false;
            }
        };

        let mut attempts = 0;

        while !self.file_appender.exists(&socket_path) {
            time::sleep(Duration::from_millis(50)).await;
            attempts += 1;
            if attempts == 5 {
                self.log.error("OpenBao started, but socket not created!");
                return false;
            }
        }

        let mut openbao_client = match SimpleOpenBaoClient::build(
            socket_path.clone(),
            Arc::clone(&openbao_logger),
        )
        .await
        {
            Ok(openbao_client) => openbao_client,
            Err(err) => {
                self.log.error(&format!(
                    "Failed to build OpenBao Client, using socket {socket_path:?}: {err}"
                ));
                return false;
            }
        };

        let secrets = match openbao_client.intialize().await {
            Ok(response) => {
                println!(
                    "{}",
                    "Record these secret keys!! They will not be shown again"
                        .italic()
                        .yellow()
                );

                println!("  Base 64                                       Key");
                println!(
                    "  ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔  ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔"
                );
                for secret in &response.secrets {
                    println!("  {}  {}", secret.base64, secret.key);
                }
                response
            }
            Err(err) => match err {
                openbao::OpenBaoError::AlreadyInitialized => {
                    self.log.warn("OpenBao already intialized!");
                    return false;
                }
                err => {
                    self.log.error(&format!("Error initializing: {err}"));
                    return false;
                }
            },
        };

        self.log.info("Unsealing OpenBao…");

        if let Err(err) = openbao_client.unseal(secrets).await {
            self.log.error(&format!("Error unsealing: {err}"));
            return false;
        }

        self.log.info("OpenBao is unsealed!");
        true
    }

    async fn initialize_docker(&self) -> bool {
        let docker_logger: Arc<dyn Logger> = Arc::new(FileLogger::new(
            &self.system_paths.log_path("docker"),
            Arc::clone(&self.file_appender),
        ));

        let mut docker_system_client = unwrap_or_bail!(
            self.create_docker_system_client(Arc::clone(&docker_logger))
                .await
        );
        let mut docker_network_client = unwrap_or_bail!(
            self.create_docker_network_client(Arc::clone(&docker_logger))
                .await
        );

        commands::InitializeDocker::new(
            &self.log,
            &mut docker_system_client,
            &mut docker_network_client,
        )
        .perform()
        .await
    }

    async fn create_docker_system_client(
        &self,
        docker_logger: Arc<dyn Logger>,
    ) -> Option<docker::SimpleSystemClient> {
        match docker::SimpleSystemClient::build(
            self.system_paths.docker_socket_path(),
            Arc::clone(&docker_logger),
        )
        .await
        {
            Ok(client) => Some(client),
            Err(err) => {
                self.log
                    .error(&format!("Failed to create Docker system client: {err}"));
                None
            }
        }
    }

    async fn create_docker_network_client(
        &self,
        docker_logger: Arc<dyn Logger>,
    ) -> Option<docker::SimpleNetworkClient> {
        match docker::SimpleNetworkClient::build(
            self.system_paths.docker_socket_path(),
            Arc::clone(&docker_logger),
        )
        .await
        {
            Ok(client) => Some(client),
            Err(err) => {
                self.log
                    .error(&format!("Failed to create Docker network client: {err}"));
                None
            }
        }
    }

    async fn create_docker_image_client(
        &self,
        docker_logger: Arc<dyn Logger>,
    ) -> Option<Box<dyn docker::ImageClient>> {
        match docker::SimpleImageClient::build(
            self.system_paths.docker_socket_path(),
            Arc::clone(&docker_logger),
        )
        .await
        {
            Ok(client) => Some(Box::new(client)),
            Err(err) => {
                self.log
                    .error(&format!("Failed to create Docker image client: {err}"));
                None
            }
        }
    }

    async fn create_docker_container_client(
        &self,
        docker_logger: Arc<dyn Logger>,
    ) -> Option<Box<dyn docker::ContainerClient>> {
        match docker::SimpleContainerClient::build(
            self.system_paths.docker_socket_path(),
            Arc::clone(&docker_logger),
        )
        .await
        {
            Ok(client) => Some(Box::new(client)),
            Err(err) => {
                self.log
                    .error(&format!("Failed to create Docker image container: {err}"));
                None
            }
        }
    }

    fn create_bract_client(&self) -> bract::Client {
        let file_reader: Box<dyn FileReader> = Box::new(LocalFileReader::new());

        let bract_logger: Box<dyn Logger> = Box::new(FileLogger::new(
            &self.system_paths.log_path("bract"),
            Arc::clone(&self.file_appender),
        ));

        bract::Client::new(
            bract_logger,
            file_reader,
            self.system_paths.douglas_socket_path("bract"),
            self.system_paths.token_path(),
        )
    }
}
