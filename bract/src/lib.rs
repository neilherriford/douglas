mod bootstrap;
mod encoding;

pub use bootstrap::service_definition;

use blueprint::listener::SocketListenerFactory;
use config::DouglasFolders;
use credentials::{CredentialsError, create_credentials};
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileSystemError, Permissions, UnixDomainSocket,
    UnixFileDeleter, UnixFolder, UnixPermissions,
};
use log::{BufferedFileReporter, Reporter, ScopeKind, Span};
use os::{Os, Unix};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use tokio::sync::broadcast::{self, Sender};

#[derive(Error, Debug)]
pub enum BractError {
    #[error("BootstrapError: {0}")]
    BootstrapError(#[from] BootstrapError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("FileSystem error: {0}")]
    FileSystemError(#[from] FileSystemError),
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
    #[error("Failed to bootstrap")]
    FailedBoostrap(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Service {
    pub name: String,
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mount {
    pub name: String,
    pub path: PathBuf,
}

pub struct Bract {
    listener_factories: Vec<SocketListenerFactory>,
    shutdown_sender: Sender<()>,
    reporter: Arc<dyn Reporter>,
}

impl Bract {
    pub async fn build(reporting_fd: i32) -> Result<Self, BractError> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = Box::new(UnixFolder::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let unix_domain_socket: Arc<dyn BindableUnixDomainSocketFile> =
            Arc::new(UnixDomainSocket::new());
        let permissions = Box::new(UnixPermissions::new());
        let douglas_folders = DouglasFolders::new();
        let (shutdown_sender, _) = broadcast::channel::<()>(1);

        bootstrap::bootstrap(
            reporting_fd,
            &*credentials,
            &*folder,
            &*permissions,
            &douglas_folders,
        )
        .await?;

        let permissions: Arc<dyn Permissions> = Arc::from(*permissions);

        let listener_factories = bootstrap::service_definition(&douglas_folders)
            .owned_sockets
            .into_iter()
            .map(|socket_definition| {
                SocketListenerFactory::new(
                    socket_definition,
                    Arc::clone(&file_deleter),
                    Arc::clone(&permissions),
                    Arc::clone(&unix_domain_socket),
                )
            })
            .collect();

        let reporter: Arc<dyn Reporter> =
            Arc::new(BufferedFileReporter::new(douglas_folders.service_log_file("bract")));

        Ok(Self {
            listener_factories,
            shutdown_sender,
            reporter,
        })
    }

    pub async fn start(&self) -> Result<(), BractError> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting bract",
            ScopeKind::Group,
        );

        let mut shutdown = self.shutdown_sender.subscribe();

        let listeners: Vec<_> = self
            .listener_factories
            .iter()
            .map(|f| f.create(&span))
            .collect::<Result<_, _>>()?;

        let reporter = Arc::clone(&self.reporter);
        let accept_loops = async move {
            let tasks: Vec<_> = listeners
                .into_iter()
                .map(|listener| {
                    let reporter = Arc::clone(&reporter);
                    tokio::spawn(async move { Self::accept_loop(listener, reporter).await })
                })
                .collect();

            for task in tasks {
                task.await.map_err(std::io::Error::other)??;
            }
            Ok::<_, BractError>(())
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
        reporter: Arc<dyn Reporter>,
    ) -> Result<(), BractError> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let reporter = Arc::clone(&reporter);
            tokio::spawn(async move {
                Self::handle_connection(stream, reporter).await;
            });
        }
    }

    async fn handle_connection(_stream: tokio::net::UnixStream, _reporter: Arc<dyn Reporter>) {
        // TODO: deserialize request, dispatch to handler, serialize response
        todo!()
    }
}
