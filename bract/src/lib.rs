mod blueprints;
mod labels;
mod protocol;
mod rolodex;

pub use blueprints::bootstrap::service_definition;
pub use blueprints::traefik_dynamic_dir;
pub use bract_types::{Mount, Request, Response, SeedlingStatus, ServerMessage, Service};
use heartbeat::{HeartbeatWriter, LocalHeartbeatWriter};

use crate::{
    labels::LabelError,
    rolodex::{FileRolodex, Rolodex, RolodexError},
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
    FolderDeleter, Inspect, Permissions, UnixDomainSocket, UnixFileDeleter, UnixFileReader,
    UnixFileWriter, UnixFolder, UnixFolderDeleter, UnixInspect, UnixLinks, UnixPermissions,
};
use log::{
    BufferedFileReporter, ChannelReporter, Reporter, ScopeKind, Span, TeeReporter,
};
use os::{Os, Unix};
use resin_client::LocalhostClientBuilder;
use seedbank_types::{DesiredRunStatus, Name, Seedling};
use std::{cmp::Ordering, sync::Arc};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    sync::broadcast::{self, Sender},
};

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
    #[error("Failed to create new seedling: {0}")]
    NewSeedlingError(#[from] blueprints::new_seedling::NewSeedlingError),
    #[error("Failed to write traefik routes: {0}")]
    WriteTraefikRoutesError(#[from] blueprints::write_traefik_routes::WriteTraefikRoutesError),
    #[error("Failed to find deadwood: {0}")]
    FindDeadwoodError(#[from] blueprints::find_deadwood::FindDeadwoodError),
    #[error("Failed to prune deadwood: {0}")]
    PruneDeadwoodError(#[from] blueprints::prune_deadwood::PruneDeadwoodError),
    #[error("Failed to determine OpenBao status: {0}")]
    OpenBaoStatusError(#[from] blueprints::openbao_status::OpenBaoStatusError),
    #[error("Failed to provision seedling secrets: {0}")]
    ProvisionSeedlingSecretsError(
        #[from] blueprints::provision_seedling_secrets::ProvisionSeedlingSecretsError,
    ),
    #[error("Name parse error: {0}")]
    NameParseError(#[from] seedbank_types::NameParseError),
    #[error("Rolodex error: {0}")]
    RolodexError(#[from] RolodexError),
}

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
    #[error("OS error: {0}")]
    OsError(#[from] os::OsError),
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
    async fn new_seedling(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &seedbank_types::Name,
        user_seedling_definition: &seedbank_types::UserSeedlingDefinition,
    ) -> Result<String, Error>;
    async fn find_deadwood(
        &self,
        reporter: Arc<dyn Reporter>,
    ) -> Result<bract_types::Deadwood, Error>;
    async fn prune_deadwood(
        &self,
        reporter: Arc<dyn Reporter>,
        deadwood: &bract_types::Deadwood,
    ) -> Result<(), Error>;
    async fn list_seedlings(&self, reporter: Arc<dyn Reporter>) -> Result<Vec<Name>, Error>;
    async fn openbao_status(
        &self,
        reporter: Arc<dyn Reporter>,
    ) -> Result<bract_types::OpenBaoReport, Error>;
    async fn stop(
        &self,
        reporter: Arc<dyn Reporter>,
        including_containers: bool,
    ) -> Result<(), Error>;
}

pub struct Bract {
    listener_factory: SocketListenerFactory,
    trigger_listener_factory: SocketListenerFactory,
    shutdown_sender: Sender<()>,
    watchdog_shutdown_sender: Sender<()>,
    watchdog_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    reporter: Arc<dyn Reporter>,
    docker_client: Arc<dyn docker::client::Client>,
    seedbank_client: Arc<dyn seedbank_client::Client>,
    credentials: Arc<dyn Credentials>,
    inspect: Arc<dyn Inspect>,
    folder: Arc<dyn Folder>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    file_deleter: Arc<dyn FileDeleter>,
    folder_deleter: Arc<dyn FolderDeleter>,
    permissions: Arc<dyn Permissions>,
    douglas_folders: DouglasFolders,
    resin_client_builder: Arc<dyn resin_client::ClientBuilder>,
    openbao_client_factory: Arc<dyn openbao::ClientFactory>,
    rolodex: Arc<dyn Rolodex>,
    registry: Registry,
    ram_disk: Arc<dyn ram_disk::RamDisk>,
    heartbeat_writer: Box<dyn HeartbeatWriter>,
}

impl<'a> From<&'a Bract> for blueprints::drop_seedling::Dependencies<'a> {
    fn from(bract: &'a Bract) -> Self {
        Self {
            docker_client: bract.docker_client.as_ref(),
            resin_client_builder: bract.resin_client_builder.as_ref(),
            seedbank_client: bract.seedbank_client.as_ref(),
            file_deleter: bract.file_deleter.as_ref(),
            folder_deleter: bract.folder_deleter.as_ref(),
            file_reader: bract.file_reader.as_ref(),
            folder: bract.folder.as_ref(),
            douglas_folders: &bract.douglas_folders,
            ram_disk: bract.ram_disk.as_ref(),
        }
    }
}

impl<'a> From<&'a Bract> for blueprints::reconcile_seedling::Dependencies<'a> {
    fn from(bract: &'a Bract) -> Self {
        Self {
            credentials: bract.credentials.as_ref(),
            inspect: bract.inspect.as_ref(),
            folder: bract.folder.as_ref(),
            file_reader: bract.file_reader.as_ref(),
            file_writer: bract.file_writer.as_ref(),
            file_deleter: bract.file_deleter.as_ref(),
            folder_deleter: bract.folder_deleter.as_ref(),
            permissions: bract.permissions.as_ref(),
            douglas_folders: &bract.douglas_folders,
            docker_client: bract.docker_client.as_ref(),
            resin_client_builder: bract.resin_client_builder.as_ref(),
            seedbank_client: bract.seedbank_client.as_ref(),
            registry: &bract.registry,
            rolodex: bract.rolodex.as_ref(),
            ram_disk: bract.ram_disk.as_ref(),
        }
    }
}

impl<'a> From<&'a Bract> for blueprints::start_seedling::Dependencies<'a> {
    fn from(bract: &'a Bract) -> Self {
        Self {
            inspect: bract.inspect.as_ref(),
            file_reader: bract.file_reader.as_ref(),
            permissions: bract.permissions.as_ref(),
            douglas_folders: &bract.douglas_folders,
            docker_client: bract.docker_client.as_ref(),
            seedbank_client: bract.seedbank_client.as_ref(),
            rolodex: bract.rolodex.as_ref(),
            registry: &bract.registry,
        }
    }
}

impl<'a> From<&'a Bract> for blueprints::write_traefik_routes::Dependencies<'a> {
    fn from(bract: &'a Bract) -> Self {
        Self {
            seedbank_client: bract.seedbank_client.as_ref(),
            docker_client: bract.docker_client.as_ref(),
            folder: bract.folder.as_ref(),
            file_writer: bract.file_writer.as_ref(),
            permissions: bract.permissions.as_ref(),
            rolodex: bract.rolodex.as_ref(),
            douglas_folders: &bract.douglas_folders,
        }
    }
}

impl Bract {
    pub async fn build(reporting_fd: i32) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = Box::new(UnixFolder::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let folder_deleter: Arc<dyn FolderDeleter> = Arc::new(UnixFolderDeleter::new());
        let unix_domain_socket: Arc<dyn BindableUnixDomainSocketFile> =
            Arc::new(UnixDomainSocket::new());
        let permissions = Box::new(UnixPermissions::new());
        let inspect = Box::new(UnixInspect::new());
        let file_writer = UnixFileWriter::new();
        let links = UnixLinks::new();
        let douglas_folders = DouglasFolders::new();
        let (shutdown_sender, _) = broadcast::channel::<()>(1);
        let (watchdog_shutdown_sender, _) = broadcast::channel::<()>(1);

        blueprints::bootstrap::bootstrap(
            reporting_fd,
            blueprints::bootstrap::Dependencies {
                credentials: &*credentials,
                folder: &*folder,
                file_writer: &file_writer,
                file_deleter: &*file_deleter,
                links: &links,
                permissions: &*permissions,
                inspect: &*inspect,
                os: &*os,
                douglas_folders: &douglas_folders,
                docker_client_builder: &docker::client::UdsClientBuilder,
            },
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
            douglas_folders.service_log_file(blueprints::bootstrap::BRACT),
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
        let ram_disk: Arc<dyn ram_disk::RamDisk> =
            Arc::from(ram_disk::create_ram_disk(Arc::clone(&os)));
        let inspect = Arc::new(UnixInspect::new());
        let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
        let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
        let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
        let resin_client_builder = Arc::new(LocalhostClientBuilder);
        let openbao_client_factory: Arc<dyn openbao::ClientFactory> =
            Arc::new(openbao::SocketClientFactory::new(Arc::clone(&reporter)));
        let rolodex = Arc::new(FileRolodex::new(
            douglas_folders.rolodex(),
            Arc::clone(&credentials),
            Arc::clone(&folder),
            Arc::clone(&file_reader),
            Arc::clone(&file_writer),
        ));

        let heartbeat_writer = Box::new(LocalHeartbeatWriter::new(
            Arc::clone(&file_writer),
            &douglas_folders.service_heartbeat_file(blueprints::bootstrap::BRACT),
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
            file_deleter,
            folder_deleter,
            permissions: Arc::clone(&permissions),
            douglas_folders: DouglasFolders::new(),
            resin_client_builder,
            openbao_client_factory,
            rolodex,
            registry: format!("localhost:{}", resin_types::DEFAULT_PORT)
                .parse()
                .map_err(|err: docker_types::RegistryError| Error::BuildError(err.to_string()))?,
            ram_disk,
            heartbeat_writer,
            watchdog_shutdown_sender,
            watchdog_task: tokio::sync::Mutex::new(None),
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
            self.as_ref().into(),
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
            let watchdog_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::watchdog_loop(server).await })
            };
            *self.watchdog_task.lock().await = Some(watchdog_task);
            let heartbeat_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::heartbeat_loop(server).await })
            };
            let log_rotation_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::log_rotation_loop(server).await })
            };

            main_task.await.map_err(std::io::Error::other)??;
            trigger_task.await.map_err(std::io::Error::other)??;
            heartbeat_task.await.map_err(std::io::Error::other)?;
            log_rotation_task.await.map_err(std::io::Error::other)?;
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

    async fn watchdog_loop(server: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutdown = server.watchdog_shutdown_sender.subscribe();

        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = shutdown.recv() => break,
            }

            let span = Span::new(
                Arc::clone(&server.reporter),
                "Running watchdog sweep",
                ScopeKind::Task,
            );

            if let Err(err) = blueprints::watchdog::execute(
                Arc::clone(&server.reporter),
                blueprints::watchdog::Dependencies {
                    docker_client: server.docker_client.as_ref(),
                    seedbank_client: server.seedbank_client.as_ref(),
                    credentials: server.credentials.as_ref(),
                    inspect: server.inspect.as_ref(),
                    folder: server.folder.as_ref(),
                    file_reader: server.file_reader.as_ref(),
                    file_writer: server.file_writer.as_ref(),
                    file_deleter: server.file_deleter.as_ref(),
                    folder_deleter: server.folder_deleter.as_ref(),
                    permissions: server.permissions.as_ref(),
                    douglas_folders: &server.douglas_folders,
                    resin_client_builder: server.resin_client_builder.as_ref(),
                    registry: &server.registry,
                    rolodex: server.rolodex.as_ref(),
                    agent_provisioning: None,
                    ram_disk: server.ram_disk.as_ref(),
                },
            )
            .await
            {
                span.message(log::Level::Warn, &format!("Watchdog sweep failed: {err}"));
            }
        }
    }

    async fn log_rotation_loop(server: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            if let Err(err) = blueprints::rotate_seedling_logs::execute(
                Arc::clone(&server.reporter),
                server.seedbank_client.as_ref(),
                server.folder.as_ref(),
                &file_system::UnixFileRotator::new(),
                &server.douglas_folders,
            )
            .await
            {
                let span = Span::new(
                    Arc::clone(&server.reporter),
                    "Rotating seedling logs",
                    ScopeKind::Task,
                );
                span.message(
                    log::Level::Warn,
                    &format!("Seedling log rotation sweep failed: {err}"),
                );
            }
        }
    }

    async fn heartbeat_loop(server: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            let span = Span::new(
                Arc::clone(&server.reporter),
                "Updating heartbeat",
                ScopeKind::Task,
            );

            if let Err(err) = server.heartbeat_writer.write() {
                span.message(log::Level::Warn, &format!("Heartbeat write failed: {err}"));
            }
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

        let is_stop_request = matches!(request, Request::StopBract { .. });

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

        if is_stop_request {
            let _ = writer.flush().await;
            let _ = server.shutdown_sender.send(());
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

        let result = self
            .do_reconcile_seedling(
                Arc::clone(&self.reporter),
                &name,
                &seedling.version,
                &seedling.definition,
                Some(&seedling.id),
            )
            .await;

        match result {
            Ok(()) => guard.finish_with_outcome(log::Outcome::Ok),
            Err(err) => {
                guard.span().message(
                    log::Level::Warn,
                    &format!("Triggered reconcile for '{name}' failed: {err}"),
                );
                guard.finish_with_outcome(log::Outcome::Failed);
            }
        }
    }

    async fn do_reconcile_seedling(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &seedbank_types::Name,
        version: &seedbank_types::Version,
        seedling_definition: &seedbank_types::SeedlingDefinition,
        seedling_id: Option<&seedbank_types::Id>,
    ) -> Result<(), Error> {
        let mut identity = identity::LocalIdentity::new(
            Arc::clone(&self.file_reader),
            Arc::clone(&self.file_writer),
        );
        let agent_provisioning =
            blueprints::provision_seedling_secrets::provision_agent_if_requested(
                self.openbao_client_factory.as_ref(),
                self.file_reader.as_ref(),
                &mut identity,
                &self.douglas_folders,
                name,
                seedling_id,
                seedling_definition,
            )
            .await
            .map_err(Error::from)?;

        blueprints::reconcile_seedling::execute(
            Arc::clone(&reporter),
            self.into(),
            name,
            version,
            seedling_definition,
            agent_provisioning.as_ref(),
        )
        .await
        .map_err(Error::from)?;

        blueprints::write_traefik_routes::execute(reporter, self.into())
            .await
            .map_err(Error::from)
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

    async fn stop_user_seedlings(&self, span: &Span) {
        let seedling_names = match self.seedbank_client.list().await {
            Ok(seedling_names) => seedling_names,
            Err(err) => {
                span.message(
                    log::Level::Warn,
                    &format!("Failed to retrieve seedling names: {err}"),
                );
                return;
            }
        };

        for seedling_name in seedling_names {
            let desired_run_status = match self
                .seedbank_client
                .get_desired_run_status(&seedling_name)
                .await
            {
                Ok(desired_run_status) => desired_run_status,
                Err(err) => {
                    span.message(
                        log::Level::Warn,
                        &format!("{seedling_name} failed to check desired run status: {err}"),
                    );
                    continue;
                }
            };

            if desired_run_status == DesiredRunStatus::Stopped {
                continue;
            }

            let status = match self
                .seedling_status(Arc::clone(&span.reporter), &seedling_name)
                .await
            {
                Ok(status) => status,
                Err(err) => {
                    span.message(
                        log::Level::Warn,
                        &format!("{seedling_name} failed to check status: {err}"),
                    );
                    continue;
                }
            };

            if !matches!(status, bract_types::SeedlingStatus::Running(_)) {
                continue;
            }

            if let Err(err) = self
                .stop_seedling(Arc::clone(&span.reporter), &seedling_name)
                .await
            {
                span.message(
                    log::Level::Warn,
                    &format!("{seedling_name} failed to stop: {err}"),
                );
            }
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

async fn stop_seedling_requested_by(
    docker_client: &dyn docker::client::Client,
    seedbank_client: &dyn seedbank_client::Client,
    reporter: Arc<dyn Reporter>,
    name: &Name,
    requested_by: blueprints::RequestedBy,
) -> Result<(), Error> {
    blueprints::stop_seedling::execute(reporter, docker_client, seedbank_client, name, requested_by)
        .await
        .map_err(Error::from)
}

async fn stop_core_seedlings(
    docker_client: &dyn docker::client::Client,
    seedbank_client: &dyn seedbank_client::Client,
    span: &Span,
) {
    for core_seedling_name in [config::seedlings::TRAEFIK, config::seedlings::OPENBAO] {
        let Ok(core_seedling) = core_seedling_name.parse::<seedbank_types::Name>() else {
            span.message(
                log::Level::Warn,
                &format!("failed to parse core seedling name '{core_seedling_name}'"),
            );
            continue;
        };

        if let Err(err) = stop_seedling_requested_by(
            docker_client,
            seedbank_client,
            Arc::clone(&span.reporter),
            &core_seedling,
            blueprints::RequestedBy::Watchdog,
        )
        .await
        {
            span.message(
                log::Level::Warn,
                &format!("{core_seedling_name} failed to stop: {err}"),
            );
        }
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
        let container_name: ContainerName = bract_types::container_name(&seedling.name)?;

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
        self.do_reconcile_seedling(reporter, name, version, seedling_definition, None)
            .await?;

        Ok(())
    }

    async fn start_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        if self.seedbank_client.exists(name).await.is_ok_and(|exists| exists)
            && self.rolodex.find_service_account(name.as_ref())?.is_none()
        {
            let seedling = self.seedbank_client.load(name).await?;
            return self
                .do_reconcile_seedling(
                    reporter,
                    name,
                    &seedling.version,
                    &seedling.definition,
                    Some(&seedling.id),
                )
                .await;
        }

        blueprints::start_seedling::execute(
            reporter,
            self.into(),
            name,
            blueprints::RequestedBy::Operator,
        )
        .await
        .map_err(Error::from)
    }

    async fn stop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        stop_seedling_requested_by(
            self.docker_client.as_ref(),
            self.seedbank_client.as_ref(),
            reporter,
            name,
            blueprints::RequestedBy::Operator,
        )
        .await
    }

    async fn drop_seedling(&self, reporter: Arc<dyn Reporter>, name: &Name) -> Result<(), Error> {
        if self
            .seedbank_client
            .exists(name)
            .await
            .map_err(|err| Error::SeedbankError(err.to_string()))?
        {
            let seedling = self
                .seedbank_client
                .load(name)
                .await
                .map_err(|err| Error::SeedbankError(err.to_string()))?;

            let mut identity = identity::LocalIdentity::new(
                Arc::clone(&self.file_reader),
                Arc::clone(&self.file_writer),
            );
            if let Err(err) = blueprints::provision_seedling_secrets::revoke_if_provisioned(
                self.openbao_client_factory.as_ref(),
                self.file_reader.as_ref(),
                &mut identity,
                &self.douglas_folders,
                name,
                &seedling.definition,
            )
            .await
            {
                let guard = Span::new(
                    Arc::clone(&reporter),
                    "Revoking OpenBao secrets",
                    ScopeKind::Step,
                )
                .start_guard();
                guard.span().message(
                    log::Level::Warn,
                    &format!(
                        "Could not revoke OpenBao secrets for '{name}': {err}. Dropping the rest of the seedling anyway; run 'seedling prune' to clean this up later."
                    ),
                );
                guard.finish_with_outcome(log::Outcome::Failed);
            }
        }

        blueprints::drop_seedling::execute(reporter, self.into(), name)
            .await
            .map_err(Error::from)
    }

    async fn new_seedling(
        &self,
        reporter: Arc<dyn Reporter>,
        name: &seedbank_types::Name,
        user_seedling_definition: &seedbank_types::UserSeedlingDefinition,
    ) -> Result<String, Error> {
        blueprints::new_seedling::execute(
            reporter,
            self.docker_client.as_ref(),
            &*self.resin_client_builder,
            self.seedbank_client.as_ref(),
            &self.registry,
            name,
            user_seedling_definition,
        )
        .await
        .map_err(Error::from)
    }

    async fn find_deadwood(
        &self,
        reporter: Arc<dyn Reporter>,
    ) -> Result<bract_types::Deadwood, Error> {
        let mut resin_client = self
            .resin_client_builder
            .build(Arc::clone(&reporter))
            .await
            .map_err(|err| Error::BuildError(err.to_string()))?;

        let mut identity = identity::LocalIdentity::new(
            Arc::clone(&self.file_reader),
            Arc::clone(&self.file_writer),
        );

        blueprints::find_deadwood::execute(blueprints::find_deadwood::Dependencies {
            seedbank_client: self.seedbank_client.as_ref(),
            docker_client: self.docker_client.as_ref(),
            resin_client: resin_client.as_mut(),
            folder: self.folder.as_ref(),
            douglas_folders: &self.douglas_folders,
            openbao_client_factory: self.openbao_client_factory.as_ref(),
            file_reader: self.file_reader.as_ref(),
            identity: &mut identity,
        })
        .await
        .map_err(Error::from)
    }

    async fn prune_deadwood(
        &self,
        reporter: Arc<dyn Reporter>,
        deadwood: &bract_types::Deadwood,
    ) -> Result<(), Error> {
        let mut resin_client = self
            .resin_client_builder
            .build(Arc::clone(&reporter))
            .await
            .map_err(|err| Error::BuildError(err.to_string()))?;

        let mut identity = identity::LocalIdentity::new(
            Arc::clone(&self.file_reader),
            Arc::clone(&self.file_writer),
        );

        blueprints::prune_deadwood::execute(
            reporter,
            blueprints::prune_deadwood::Dependencies {
                docker_client: self.docker_client.as_ref(),
                resin_client: resin_client.as_mut(),
                file_deleter: self.file_deleter.as_ref(),
                folder_deleter: self.folder_deleter.as_ref(),
                openbao_client_factory: self.openbao_client_factory.as_ref(),
                file_reader: self.file_reader.as_ref(),
                identity: &mut identity,
                douglas_folders: &self.douglas_folders,
            },
            deadwood,
        )
        .await
        .map_err(Error::from)
    }

    async fn list_seedlings(&self, _reporter: Arc<dyn Reporter>) -> Result<Vec<Name>, Error> {
        self.seedbank_client
            .list()
            .await
            .map_err(|err| Error::SeedbankError(err.to_string()))
    }

    async fn openbao_status(
        &self,
        reporter: Arc<dyn Reporter>,
    ) -> Result<bract_types::OpenBaoReport, Error> {
        let openbao_name: Name = openbao::SEEDLING_NAME.parse()?;
        let is_running = matches!(
            self.seedling_status(Arc::clone(&reporter), &openbao_name)
                .await?,
            SeedlingStatus::Running(..)
        );

        let mut identity = identity::LocalIdentity::new(
            Arc::clone(&self.file_reader),
            Arc::clone(&self.file_writer),
        );

        blueprints::openbao_status::execute(
            self.openbao_client_factory.as_ref(),
            self.file_reader.as_ref(),
            &mut identity,
            is_running,
            &self.douglas_folders,
        )
        .await
        .map_err(Error::from)
    }

    async fn stop(
        &self,
        reporter: Arc<dyn Reporter>,
        including_containers: bool,
    ) -> Result<(), Error> {
        let span = Span::new(Arc::clone(&reporter), "Stopping bract…", ScopeKind::Step);

        let _ = self.watchdog_shutdown_sender.send(());
        if let Some(handle) = self.watchdog_task.lock().await.take()
            && let Err(err) = handle.await
        {
            span.message(log::Level::Warn, &format!("watchdog task panicked: {err}"));
        }

        if !including_containers {
            return Ok(());
        }

        stop_core_seedlings(
            self.docker_client.as_ref(),
            self.seedbank_client.as_ref(),
            &span,
        )
        .await;
        self.stop_user_seedlings(&span).await;

        Ok(())
    }
}

#[cfg(test)]
mod stop_core_seedlings_tests {
    use super::*;
    use docker::client::ContainerRef;

    fn core_seedling_name(raw: &str) -> Name {
        raw.parse().expect("valid seedling name")
    }

    fn test_reporter() -> Arc<dyn Reporter> {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(log::ChannelReporter::new(sender))
    }

    fn existing_stopped_core_seedling_docker_client(name: &Name) -> docker::MockClient {
        let main = bract_types::container_name(name).expect("valid container name");

        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_container_exists()
            .returning(move |container_ref| {
                let ContainerRef::FullName(container_name) = container_ref else {
                    return Ok(false);
                };
                Ok(container_name == main)
            });
        docker_client
            .expect_container_status()
            .returning(|_| Ok(docker_types::Status::Exited));
        docker_client.expect_container_labels().returning(|_| {
            Ok(vec![crate::labels::create_origin_label(
                seedbank_types::Origin::Core,
            )])
        });
        docker_client
    }

    fn accepting_seedbank_client() -> seedbank_client::MockClient {
        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_set_desired_run_status()
            .returning(|_, _| Ok(()));
        seedbank_client
    }

    #[tokio::test]
    async fn test_stop_seedling_requested_by_should_allow_watchdog_to_stop_a_core_seedling() {
        let name = core_seedling_name("traefik");
        let docker_client = existing_stopped_core_seedling_docker_client(&name);
        let seedbank_client = accepting_seedbank_client();

        let result = stop_seedling_requested_by(
            &docker_client,
            &seedbank_client,
            test_reporter(),
            &name,
            blueprints::RequestedBy::Watchdog,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stop_seedling_requested_by_should_forbid_operator_from_stopping_a_core_seedling()
     {
        let name = core_seedling_name("traefik");
        let docker_client = existing_stopped_core_seedling_docker_client(&name);
        let seedbank_client = accepting_seedbank_client();

        let result = stop_seedling_requested_by(
            &docker_client,
            &seedbank_client,
            test_reporter(),
            &name,
            blueprints::RequestedBy::Operator,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stop_core_seedlings_should_stop_both_traefik_and_openbao() {
        let mut docker_client = docker::MockClient::new();
        docker_client.expect_container_exists().returning(|_| Ok(true));
        docker_client
            .expect_container_status()
            .returning(|_| Ok(docker_types::Status::Exited));
        docker_client.expect_container_labels().returning(|_| {
            Ok(vec![crate::labels::create_origin_label(
                seedbank_types::Origin::Core,
            )])
        });

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_set_desired_run_status()
            .times(2)
            .returning(|_, _| Ok(()));

        let span = Span::new(test_reporter(), "test", ScopeKind::Group);

        stop_core_seedlings(&docker_client, &seedbank_client, &span).await;
    }
}
