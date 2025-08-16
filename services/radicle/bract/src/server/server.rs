use super::active_mount_version::ActiveMountVersion;
use super::create_credentials::CreateCredentials;
use super::create_listener::CreateListener;
use super::create_mount::CreateMount;
use super::create_new_mount_version::CreateNewMountVersion;
use super::create_system_credentials::CreateSystemCredentials;
use super::list_mount_versions::ListMountVersions;
use super::mount_path_factory::MountPathFactory;
use super::set_mount_version::SetMountVersion;
use super::token_refresher::TokenRefresher;
use super::token_validator::TokenValidator;
use super::version_manager::VersionManager;
use crate::{Request, Response};
use credentials::{Credentials, CredentialsError, create_for_target};
use file_system::{
    FileDeleter, FileReader, FileSystemError, FileWriter, Folder, Links, Listener, Permissions,
    UnixDomainSocket,
};
use futures::{SinkExt, StreamExt};
use log::Logger;
use os::{Os, OsError};
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time;
use tokio_serde::Framed as SerdeFramed;
use tokio_serde::formats::Json;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

struct RequestHandler {
    active_mount_version: ActiveMountVersion,
    create_credentials: CreateCredentials,
    create_mount: CreateMount,
    create_new_mount_version: CreateNewMountVersion,
    list_mount_versions: ListMountVersions,
    set_mount_version: SetMountVersion,
}

impl RequestHandler {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        folder: Arc<dyn Folder + Sync + Send + 'static>,
        file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
        file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
        links: Arc<dyn Links + Sync + Send + 'static>,
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
        permissions: Arc<dyn Permissions + Sync + Send + 'static>,
        token_path: &Path,
        mount_root: &Path,
    ) -> Self {
        let token_validator = Arc::new(TokenValidator::new(
            Arc::clone(&log),
            Arc::clone(&file_reader),
            &token_path,
        ));

        let mount_path_factory = Arc::new(MountPathFactory::new(
            mount_root,
            Arc::clone(&folder),
            Arc::clone(&links),
        ));

        let version_manager = Arc::new(VersionManager::new(
            Arc::clone(&mount_path_factory),
            Arc::clone(&folder),
            Arc::clone(&links),
            Arc::clone(&file_deleter),
            Arc::clone(&permissions),
            Arc::clone(&credentials),
        ));

        Self {
            active_mount_version: ActiveMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&mount_path_factory),
            ),
            create_credentials: CreateCredentials::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&credentials),
            ),
            create_mount: CreateMount::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            create_new_mount_version: CreateNewMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            list_mount_versions: ListMountVersions::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
            set_mount_version: SetMountVersion::new(
                Arc::clone(&log),
                Arc::clone(&token_validator),
                Arc::clone(&version_manager),
            ),
        }
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::ActiveMountVersion {
                token,
                service_name,
                mount_name,
            } => self
                .active_mount_version
                .perform(token, service_name, mount_name),
            Request::CreateCredentials {
                token,
                service_name,
            } => self.create_credentials.create(token, service_name),
            Request::CreateMount {
                token,
                service_name,
                mount_name,
            } => self.create_mount.create(token, service_name, mount_name),
            Request::CreateNewMountVersion {
                token,
                service_name,
                mount_name,
            } => self
                .create_new_mount_version
                .create(token, service_name, mount_name),
            Request::ListMountVersions {
                token,
                service_name,
                mount_name,
            } => self
                .list_mount_versions
                .list(token, service_name, mount_name),
            Request::SetMountVersion {
                token,
                service_name,
                mount_name,
                version,
            } => self
                .set_mount_version
                .perform(token, service_name, mount_name, version),
        }
    }
}

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Must be root")]
    NotRootError,
    #[error("OS error {0}")]
    OsError(#[from] OsError),
    #[error("FileSystemError: {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] CredentialsError),
}

pub(crate) static FIVE_MINUTES: u64 = 5 * 60;

pub struct Server {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    request_handler: Arc<RequestHandler>,
    token_refresher: Arc<TokenRefresher>,
    create_system_directory_entries: CreateSystemCredentials,
    create_listener: CreateListener,
    credentials: Arc<dyn Credentials + 'static>,
}

impl Server {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        file_reader: Arc<dyn FileReader + Sync + Send + 'static>,
        file_writer: Arc<dyn FileWriter + Sync + Send + 'static>,
        file_deleter: Arc<dyn FileDeleter + Sync + Send + 'static>,
        folder: Arc<dyn Folder + Sync + Send + 'static>,
        links: Arc<dyn Links + Sync + Send + 'static>,
        os: Arc<dyn Os + Sync + Send + 'static>,
        permissions: Arc<dyn Permissions + Sync + Send + 'static>,
        unix_domain_socket: Arc<dyn UnixDomainSocket + 'static>,
        token_path: &Path,
        socket_path: &Path,
        mount_root: &Path,
    ) -> Self {
        let credentials = create_for_target(Arc::clone(&os));
        Self {
            log: Arc::clone(&log),
            request_handler: Arc::new(RequestHandler::new(
                Arc::clone(&log),
                Arc::clone(&folder),
                Arc::clone(&file_reader),
                Arc::clone(&file_deleter),
                Arc::clone(&links),
                Arc::clone(&credentials),
                Arc::clone(&permissions),
                token_path,
                mount_root,
            )),
            token_refresher: Arc::new(TokenRefresher::new(
                Arc::clone(&log),
                token_path,
                Arc::clone(&permissions),
                file_writer,
                Arc::clone(&os),
            )),
            create_system_directory_entries: CreateSystemCredentials::new(
                Arc::clone(&log),
                Arc::clone(&credentials),
            ),
            create_listener: CreateListener::new(
                Arc::clone(&log),
                socket_path,
                Arc::clone(&file_deleter),
                Arc::clone(&permissions),
                unix_domain_socket,
            ),
            credentials,
        }
    }

    pub fn start(&self) -> Result<(), ServerError> {
        self.log.info("Starting server");
        self.assert_root()?;
        self.create_system_directory_entries.create()?;
        let log = Arc::clone(&self.log);

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let listener = self.create_listener.create()?;

            let token_refresh = Self::token_refresh_task(Arc::clone(&self.token_refresher));
            let request_handler = Self::request_handler_task(
                listener,
                Arc::clone(&log),
                Arc::clone(&self.request_handler),
            );

            tokio::try_join!(token_refresh, request_handler)?;

            Ok(())
        })
    }

    async fn request_handler_task(
        listener: Box<dyn Listener + Send + Sync + 'static>,
        log: Arc<dyn Logger + Send + Sync + 'static>,
        handler: Arc<RequestHandler>,
    ) -> Result<(), ServerError> {
        log.info("Listening…");
        loop {
            let (socket, _) = listener.accept().await?;
            let log = Arc::clone(&log);
            let handler = Arc::clone(&handler);

            tokio::spawn(async move {
                let length_delimited = Framed::new(socket, LengthDelimitedCodec::new());
                let mut transport =
                    SerdeFramed::new(length_delimited, Json::<Request, Response>::default());

                match transport.next().await {
                    Some(Ok(request)) => {
                        log.info(&format!("Received request {}", request));
                        let response = handler.handle(request);
                        let _ = transport.send(response).await;
                        log.info("Completed request");
                    }
                    None => {
                        log.warn("Invalid request");
                        let _ = transport
                            .send(Response::Error("Invalid request".to_string()))
                            .await;
                    }
                    Some(Err(err)) => {
                        let message = format!("Invalid request: {:?}", err);
                        log.error(&message);
                        let _ = transport.send(Response::Error(message.to_string())).await;
                    }
                }
            });
        }
    }

    async fn token_refresh_task(token_refresher: Arc<TokenRefresher>) -> Result<(), ServerError> {
        loop {
            token_refresher.refresh();
            time::sleep(Duration::from_secs(FIVE_MINUTES)).await;
        }
    }

    fn assert_root(&self) -> Result<(), ServerError> {
        if self.credentials.is_root() {
            Ok(())
        } else {
            self.log.error("Not root!");
            Err(ServerError::NotRootError)
        }
    }
}
