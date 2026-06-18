mod blob_store;
mod bootstrap;

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::get,
};
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    FileDeleter, FileRenamer, FileSystemError, Folder, UnixFileDeleter, UnixFileRenamer, UnixFolder,
};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span};
use os::{Os, Unix};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Cannot be root")]
    CannotBeRoot,
    #[error("Missing be root path")]
    MissingRootPath,
    #[error("IO Error {0}")]
    IoError(#[from] std::io::Error),
    #[error("File system error {0}")]
    FileSystemError(#[from] FileSystemError),
    #[error("Failed to bootstrap")]
    FailedBoostrap(Vec<String>),
}

pub const DEFAULT_PORT: u16 = 7376;

pub struct Server {
    reporter: Arc<dyn Reporter>,
    port: u16,
    blob_root: PathBuf,
    repositories_root: PathBuf,
    file_renamer: Arc<dyn FileRenamer>,
    file_deleter: Arc<dyn FileDeleter>,
    folder: Arc<dyn Folder>,
}

impl Server {
    pub async fn build(reporting_fd: i32, port: u16) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder = Arc::new(UnixFolder::new());
        let douglas_folders = DouglasFolders::new();
        let file_renamer: Arc<dyn FileRenamer> = Arc::new(UnixFileRenamer::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());

        let log_path = douglas_folders.log_file("resin");
        let root_path = douglas_folders.resin.clone();
        let mut blob_root = root_path.clone();
        blob_root.push("blobs");
        let mut repositories_root = root_path.clone();
        repositories_root.push("repositories");

        bootstrap::bootstrap(
            reporting_fd,
            &*credentials,
            &*folder,
            log_path.clone(),
            root_path.clone(),
            blob_root.clone(),
            repositories_root.clone(),
        )
        .await?;

        let reporter: Arc<dyn Reporter> = Arc::new(BufferedFileReporter::new(log_path));

        Ok(Self {
            reporter,
            port,
            blob_root,
            repositories_root,
            file_renamer,
            file_deleter,
            folder,
        })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let app = Router::new()
            .route("/v2/", get(v2))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&self.reporter),
                log_request,
            ))
            .with_state(Arc::clone(&self.reporter));

        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", self.port))
            .await
            .unwrap();

        guard.span().message(
            log::Level::Info,
            &format!("listening on {:?}", listener.local_addr()),
        );

        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn log_request(
    State(reporter): State<Arc<dyn Reporter>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let response = next.run(req).await;

    let outcome = if response.status().is_success() {
        Outcome::Ok
    } else {
        Outcome::Failed
    };
    Span::record(
        reporter,
        &format!("{method} {uri} → {}", response.status()),
        ScopeKind::Task,
        outcome,
    );

    response
}

async fn v2() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
        "hello world",
    )
}
