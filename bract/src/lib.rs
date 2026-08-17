mod blueprints;
mod labels;
mod protocol;
mod rolodex;

pub use blueprints::bootstrap::service_definition;
pub use bract_types::{Mount, Request, Response, SeedlingStatus, ServerMessage, Service};

use crate::{
    labels::LabelError,
    rolodex::{FileRolodex, Rolodex},
};
use async_trait::async_trait;
use blueprint::listener::SocketListenerFactory;
use config::DouglasFolders;
use credentials::{Credentials, CredentialsError, create_credentials};
use docker::{
    DockerError,
    client::{ClientBuilder, ContainerRef, UdsClientBuilder},
};
use docker_types::{ContainerName, Registry};
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileReader, FileSystemError, FileWriter, Folder,
    Inspect, Permissions, UnixDomainSocket, UnixFileDeleter, UnixFileReader, UnixFileWriter,
    UnixFolder, UnixInspect, UnixPermissions,
};
use log::{BufferedFileReporter, ChannelReporter, Reporter, ScopeKind, Span, TeeReporter};
use os::{Os, Unix};
use resin_client::LocalhostClientBuilder;
use seedbank_types::{Name, Seedling};
use std::{cmp::Ordering, sync::Arc};
use thiserror::Error;
use tokio::sync::broadcast::{self, Sender};

#[derive(Error, Debug)]
pub enum Error {
    #[error("BootstrapError: {0}")]
    BootstrapError(#[from] BootstrapError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("FileSystem error: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Build error: {0}")]
    BuildError(String),
    #[error("Name error: {0}")]
    NameError(String),
    #[error("Docker error: {0}")]
    DockerError(String),
    #[error("Unknown seedling")]
    UnknownSeedling,
    #[error("Too many seedlings")]
    TooManySeedlings,
    #[error("Seedbank error: {0}")]
    SeedbankError(String),
    #[error("Missing version")]
    MissingVersion,
    #[error("Seedling not yet created")]
    UncreatedSeedling,
    #[error("Label error: {0}")]
    LabelError(#[from] LabelError),
    #[error("Failed to reconcile seedling: {0}")]
    ReconcileSeedlingError(#[from] blueprints::reconcile_seedling::ReconcileSeedlingError),
    #[error("Failed to start seedling: {0}")]
    StartSeedlingError(#[from] blueprints::start_seedling::StartSeedlingError),
    #[error("Failed to stop seedling: {0}")]
    StopSeedlingError(#[from] blueprints::stop_seedling::StopSeedlingError),
    #[error("Failed to drop seedling: {0}")]
    DropSeedlingError(#[from] blueprints::drop_seedling::DropSeedlingError),
    #[error("Failed to write traefik routes: {0}")]
    WriteTraefikRoutesError(#[from] blueprints::write_traefik_routes::WriteTraefikRoutesError),
}

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Docker must be running")]
    MustHaveRunningDocker,
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Server: Send + Sync {
    async fn seedling_status(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &Name,
    ) -> Result<SeedlingStatus, Error>;
    async fn reconcile_seedling(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &seedbank_types::Name,
        version: &seedbank_types::Version,
        seedling_definition: &seedbank_types::SeedlingDefinition,
    ) -> Result<(), Error>;
    async fn start_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error>;
    async fn stop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error>;
    async fn drop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error>;
}

pub struct Bract {
    listener_factory: SocketListenerFactory,
    trigger_listener_factory: SocketListenerFactory,
    shutdown_sender: Sender<()>,
    reporter: Arc<dyn Reporter>,
    docker_client: Arc<dyn docker::client::Client>,
    seedbank_client: Arc<dyn seedbank_client::Client>,
    credentials: Arc<dyn Credentials>,
    inspect: Arc<dyn Inspect>,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    permissions: Arc<dyn Permissions>,
    douglas_folders: DouglasFolders,
    docker_client_builder: Arc<dyn ClientBuilder>,
    resin_client_builder: Arc<dyn resin_client::ClientBuilder>,
    rolodex: Arc<dyn Rolodex>,
    registry: Registry,
}

impl Bract {
    pub async fn build(reporting_fd: i32) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = Box::new(UnixFolder::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let unix_domain_socket: Arc<dyn BindableUnixDomainSocketFile> =
            Arc::new(UnixDomainSocket::new());
        let permissions = Box::new(UnixPermissions::new());
        let douglas_folders = DouglasFolders::new();
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        blueprints::bootstrap::bootstrap(
            reporting_fd,
            &*credentials,
            &*folder,
            &*permissions,
            &douglas_folders,
            &docker::client::UdsClientBuilder,
        )
        .await?;

        let permissions: Arc<dyn Permissions> = Arc::from(*permissions);

        let mut owned_sockets = blueprints::bootstrap::service_definition(&douglas_folders)
            .owned_sockets
            .into_iter();
        let listener_factory = SocketListenerFactory::new(
            owned_sockets
                .next()
                .expect("bract always defines its main control socket"),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&unix_domain_socket),
        );
        let trigger_listener_factory = SocketListenerFactory::new(
            owned_sockets
                .next()
                .expect("bract always defines its trigger socket"),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&unix_domain_socket),
        );

        let reporter: Arc<dyn Reporter> = Arc::new(BufferedFileReporter::new(
            douglas_folders.service_log_file("bract"),
        ));

        let docker_client: Arc<dyn docker::client::Client> = Arc::from(
            UdsClientBuilder {}
                .build(Arc::clone(&reporter))
                .await
                .map_err(|err| Error::BuildError(err.to_string()))?,
        );

        let seedbank_client: Arc<dyn seedbank_client::Client> = Arc::new(
            seedbank_client::UdsClient::new(Arc::clone(&reporter), &douglas_folders),
        );

        let credentials = Arc::from(create_credentials(Arc::clone(&os)));
        let inspect = Arc::new(UnixInspect::new());
        let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
        let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
        let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
        let docker_client_builder = Arc::new(UdsClientBuilder);
        let resin_client_builder = Arc::new(LocalhostClientBuilder);
        let rolodex = Arc::new(FileRolodex::new(
            douglas_folders.rolodex.clone(),
            Arc::clone(&credentials),
            Arc::clone(&folder),
            Arc::clone(&file_reader),
            Arc::clone(&file_writer),
        ));

        Ok(Self {
            listener_factory,
            trigger_listener_factory,
            shutdown_sender,
            reporter,
            docker_client,
            seedbank_client,
            credentials,
            inspect,
            folder,
            file_reader,
            file_writer,
            permissions: Arc::clone(&permissions),
            douglas_folders: DouglasFolders::new(),
            docker_client_builder,
            resin_client_builder,
            rolodex,
            registry: format!("localhost:{}", resin_types::DEFAULT_PORT)
                .parse()
                .map_err(|err: docker_types::RegistryError| Error::BuildError(err.to_string()))?,
        })
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Error> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting bract",
            ScopeKind::Group,
        );

        let mut shutdown = self.shutdown_sender.subscribe();

        if let Err(err) = blueprints::write_traefik_routes::execute(
            Arc::clone(&self.reporter),
            self.seedbank_client.as_ref(),
            self.docker_client.as_ref(),
            self.folder.as_ref(),
            self.file_writer.as_ref(),
            &*self.permissions,
            &*self.rolodex,
            &self.douglas_folders,
        )
        .await
        {
            span.message(
                log::Level::Warn,
                &format!("Could not reconstruct traefik routes at startup: {err}"),
            );
        }

        let listener = self.listener_factory.create(&span)?;
        let trigger_listener = self.trigger_listener_factory.create(&span)?;

        let accept_loops = async move {
            let main_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::accept_loop(listener, server).await })
            };
            let trigger_task = {
                let server = Arc::clone(&self);
                tokio::spawn(
                    async move { Self::accept_trigger_loop(trigger_listener, server).await },
                )
            };

            main_task.await.map_err(std::io::Error::other)??;
            trigger_task.await.map_err(std::io::Error::other)??;
            Ok::<_, Error>(())
        };

        tokio::select! {
            r = accept_loops => r?,
            _ = shutdown.recv() => {},
        }

        span.create_scoped_reporter().finish(log::Outcome::Ok);
        Ok(())
    }

    async fn accept_loop(
        listener: Box<dyn file_system::Listener + Send + Sync + 'static>,
        server: Arc<Self>,
    ) -> Result<(), Error> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let server = Arc::clone(&server);

            tokio::spawn(async move {
                Self::handle_connection(stream, server).await;
            });
        }
    }

    async fn handle_connection(mut stream: tokio::net::UnixStream, server: Arc<Self>) {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();

        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };

        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(err) => {
                let _ = Self::write_message(
                    &mut writer,
                    &ServerMessage::Response(Response::Error {
                        message: err.to_string(),
                    }),
                )
                .await;
                return;
            }
        };

        let (event_sender, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let per_request_reporter: Arc<dyn Reporter> = Arc::new(TeeReporter::new(vec![
            Box::new(Arc::clone(&server.reporter)),
            Box::new(ChannelReporter::new(event_sender)),
        ]));

        let handler = protocol::handle(server.as_ref(), per_request_reporter, request);
        tokio::pin!(handler);

        let response = loop {
            tokio::select! {
                biased;
                Some(event) = event_receiver.recv() => {
                    if let Err(err) = Self::write_message(&mut writer, &ServerMessage::Event(event)).await {
                        Self::log_connection_error(&server, "Failed to write event message", &err);
                        return;
                    }
                }
                response = &mut handler => {
                    while let Some(event) = event_receiver.recv().await {
                        if let Err(err) = Self::write_message(&mut writer, &ServerMessage::Event(event)).await {
                            Self::log_connection_error(&server, "Failed to write event message", &err);
                            return;
                        }
                    }
                    break response;
                }
            }
        };

        if let Err(err) = Self::write_message(&mut writer, &ServerMessage::Response(response)).await
        {
            Self::log_connection_error(&server, "Failed to write response message", &err);
        }
    }

    fn log_connection_error(server: &Arc<Self>, label: &str, err: &impl std::fmt::Display) {
        Span::new(
            Arc::clone(&server.reporter),
            "Handling connection",
            ScopeKind::Task,
        )
        .message(log::Level::Warn, &format!("{label}: {err}"));
    }

    async fn accept_trigger_loop(
        listener: Box<dyn file_system::Listener + Send + Sync + 'static>,
        server: Arc<Self>,
    ) -> Result<(), Error> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let server = Arc::clone(&server);

            tokio::spawn(async move {
                Self::handle_trigger_connection(stream, server).await;
            });
        }
    }

    async fn handle_trigger_connection(mut stream: tokio::net::UnixStream, server: Arc<Self>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();

        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };

        let Ok(request) = serde_json::from_str::<reconcile_trigger_types::Request>(&line) else {
            return;
        };

        let response = match request.name.parse::<seedbank_types::Name>() {
            Ok(name) => {
                let server = Arc::clone(&server);
                tokio::spawn(async move { server.trigger_reconcile(name).await });
                reconcile_trigger_types::Response::Accepted
            }
            Err(_) => reconcile_trigger_types::Response::InvalidName,
        };

        let serialized = match serde_json::to_string(&response) {
            Ok(serialized) => serialized,
            Err(err) => {
                Self::log_connection_error(&server, "Failed to serialize trigger response", &err);
                return;
            }
        };

        if let Err(err) = writer.write_all(format!("{serialized}\n").as_bytes()).await {
            Self::log_connection_error(&server, "Failed to write trigger response", &err);
        }
    }

    async fn trigger_reconcile(self: Arc<Self>, name: seedbank_types::Name) {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            &format!("Triggered reconcile for '{name}'"),
            ScopeKind::Task,
        )
        .start_guard();

        let seedling = match self.seedbank_client.load(&name).await {
            Ok(seedling) => seedling,
            Err(err) => {
                guard.span().message(
                    log::Level::Warn,
                    &format!("Could not load '{name}' for triggered reconcile: {err}"),
                );
                guard.finish_with_outcome(log::Outcome::Failed);
                return;
            }
        };

        let result = blueprints::reconcile_seedling::execute(
            Arc::clone(&self.reporter),
            &*self.credentials,
            &*self.inspect,
            &*self.folder,
            &*self.file_reader,
            &*self.file_writer,
            &*self.permissions,
            &self.douglas_folders,
            &*self.docker_client_builder,
            &*self.resin_client_builder,
            self.seedbank_client.as_ref(),
            &self.registry,
            &*self.rolodex,
            &name,
            &seedling.version,
            &seedling.definition,
            labels::Origin::User,
        )
        .await;

        match result {
            Ok(()) => {
                if let Err(err) = blueprints::write_traefik_routes::execute(
                    Arc::clone(&self.reporter),
                    self.seedbank_client.as_ref(),
                    self.docker_client.as_ref(),
                    self.folder.as_ref(),
                    self.file_writer.as_ref(),
                    &*self.permissions,
                    &*self.rolodex,
                    &self.douglas_folders,
                )
                .await
                {
                    guard.span().message(
                        log::Level::Warn,
                        &format!(
                            "Could not write traefik routes after reconciling '{name}': {err}"
                        ),
                    );
                }
                guard.finish_with_outcome(log::Outcome::Ok);
            }
            Err(err) => {
                guard.span().message(
                    log::Level::Warn,
                    &format!("Triggered reconcile for '{name}' failed: {err}"),
                );
                guard.finish_with_outcome(log::Outcome::Failed);
            }
        }
    }

    async fn write_message(
        writer: &mut (impl tokio::io::AsyncWrite + Unpin),
        message: &ServerMessage,
    ) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;

        let mut serialized = serde_json::to_string(message)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        serialized.push('\n');
        writer.write_all(serialized.as_bytes()).await
    }

    fn check_container_status(
        &self,
        container: docker_types::ContainerSnapshot,
        version: &seedbank_types::Version,
    ) -> Result<SeedlingStatus, Error> {
        let actual_version = labels::get_version(&container.definition.labels)?;

        let definition_status = match actual_version.cmp(version) {
            Ordering::Equal => bract_types::DefinitionStatus::Current,
            Ordering::Less => bract_types::DefinitionStatus::Stale,
            Ordering::Greater => bract_types::DefinitionStatus::Newer,
        };

        match container.runtime_state.status {
            docker_types::Status::Running => Ok(SeedlingStatus::Running(definition_status)),
            docker_types::Status::Exited | docker_types::Status::Created => {
                Ok(SeedlingStatus::Defined(definition_status))
            }
            _ => Ok(SeedlingStatus::Missing),
        }
    }

    async fn load_seedling(&self, name: &seedbank_types::Name) -> Result<Seedling, Error> {
        if self.seedbank_client.exists(name).await? {
            Ok(self.seedbank_client.load(name).await?)
        } else {
            Err(Error::UnknownSeedling)
        }
    }
}

impl From<seedbank_client::Error> for Error {
    fn from(value: seedbank_client::Error) -> Self {
        Error::SeedbankError(value.to_string())
    }
}

impl From<docker_types::DockerNameError> for Error {
    fn from(value: docker_types::DockerNameError) -> Self {
        Error::NameError(value.to_string())
    }
}

#[async_trait]
impl Server for Bract {
    async fn seedling_status(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &Name,
    ) -> Result<SeedlingStatus, Error> {
        let guard = Span::new(reporter, "Fetching seedling status", ScopeKind::Task).start_guard();

        let seedling = match self.load_seedling(name).await {
            Ok(seedling) => seedling,
            Err(Error::UnknownSeedling) => return guard.finish(Ok(SeedlingStatus::Unknown)),
            Err(err) => return guard.finish(Err(err)),
        };
        let container_name: ContainerName = blueprints::container_name(&seedling.name)?;

        let mount_names = seedling
            .definition
            .mounts
            .iter()
            .map(|(name, mount)| {
                let mount_name: docker_types::MountName = name.as_ref().parse()?;
                Ok((mount.remote_path().to_path_buf(), mount_name))
            })
            .collect::<Result<std::collections::HashMap<_, _>, docker_types::DockerNameError>>()?;

        guard.finish(
            match self
                .docker_client
                .inspect_container(ContainerRef::FullName(container_name), &mount_names)
                .await
            {
                Ok(container) => Ok(self.check_container_status(container, &seedling.version)?),
                Err(DockerError::ResourceNotFound) => Ok(SeedlingStatus::Missing),
                Err(err) => return Err(Error::DockerError(err.to_string())),
            },
        )
    }

    async fn reconcile_seedling(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &seedbank_types::Name,
        version: &seedbank_types::Version,
        seedling_definition: &seedbank_types::SeedlingDefinition,
    ) -> Result<(), Error> {
        blueprints::reconcile_seedling::execute(
            Arc::clone(&reporter),
            &*self.credentials,
            &*self.inspect,
            &*self.folder,
            &*self.file_reader,
            &*self.file_writer,
            &*self.permissions,
            &self.douglas_folders,
            &*self.docker_client_builder,
            &*self.resin_client_builder,
            self.seedbank_client.as_ref(),
            &self.registry,
            &*self.rolodex,
            name,
            version,
            seedling_definition,
            labels::Origin::Core,
        )
        .await
        .map_err(Error::from)?;

        blueprints::write_traefik_routes::execute(
            reporter,
            self.seedbank_client.as_ref(),
            self.docker_client.as_ref(),
            self.folder.as_ref(),
            self.file_writer.as_ref(),
            &*self.permissions,
            &*self.rolodex,
            &self.douglas_folders,
        )
        .await
        .map_err(Error::from)
    }

    async fn start_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        blueprints::start_seedling::execute(
            reporter,
            &*self.inspect,
            &*self.file_reader,
            &*self.permissions,
            &self.douglas_folders,
            &*self.docker_client_builder,
            self.seedbank_client.as_ref(),
            &*self.rolodex,
            &self.registry,
            name,
        )
        .await
        .map_err(Error::from)
    }

    async fn stop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        blueprints::stop_seedling::execute(reporter, &*self.docker_client_builder, name)
            .await
            .map_err(Error::from)
    }

    async fn drop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        blueprints::drop_seedling::execute(reporter, &*self.docker_client_builder, name)
            .await
            .map_err(Error::from)
    }
}
