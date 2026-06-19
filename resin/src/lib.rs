mod blob_store;
mod bootstrap;
mod digest;
mod tag_store;

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, head},
};
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder, UnixFileDeleter,
    UnixFileReader, UnixFileRenamer, UnixFileWriter, UnixFolder,
};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

use crate::blob_store::{BlobStore, FileBlobStore};

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
    blob_store: Arc<dyn BlobStore>,
}

#[derive(Clone)]
struct AppState {
    reporter: Arc<dyn Reporter>,
    blob_store: Arc<dyn BlobStore>,
}

impl Server {
    pub async fn build(reporting_fd: Option<i32>, port: u16) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
        let douglas_folders = DouglasFolders::new();
        let file_renamer: Arc<dyn FileRenamer> = Arc::new(UnixFileRenamer::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
        let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());

        let (root_path, log_path) = if reporting_fd.is_none() {
            let mut root = std::env::temp_dir();
            root.push("douglas-resin-dbg");
            (root, douglas_folders.log_file("resin"))
        } else {
            (
                douglas_folders.resin.clone(),
                douglas_folders.log_file("resin"),
            )
        };

        let mut blob_root = root_path.clone();
        blob_root.push("blobs");
        let mut repositories_root = root_path.clone();
        repositories_root.push("repositories");

        let reporter: Arc<dyn Reporter> = if reporting_fd.is_none() {
            let tui = TuiReporter::start()?;
            for dir in [&root_path, &blob_root, &repositories_root] {
                if !folder.exists(dir) {
                    folder.create_recursively(dir)?;
                }
            }
            Arc::new(tui)
        } else {
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
            Arc::new(BufferedFileReporter::new(log_path))
        };

        let blob_store: Arc<dyn BlobStore> = Arc::new(FileBlobStore::new(
            Arc::clone(&folder),
            Arc::clone(&file_writer),
            Arc::clone(&file_reader),
            Arc::clone(&file_renamer),
            Arc::clone(&file_deleter),
            blob_root.clone(),
        ));

        Ok(Self {
            reporter,
            port,
            blob_root,
            repositories_root,
            file_renamer,
            file_deleter,
            folder,
            blob_store,
        })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let state = AppState {
            reporter: Arc::clone(&self.reporter),
            blob_store: Arc::clone(&self.blob_store),
        };

        let app = Router::new();
        let app = app.route("/v2", get(v2));
        let app = app.route("/v2/{name}/blobs/{digest}", head(head_blob));
        let app = app.layer(middleware::from_fn_with_state(state.clone(), log_request));
        let app = app.with_state(state);

        let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", self.port)).await
        {
            Ok(l) => l,
            Err(e) => {
                guard.span().message(
                    log::Level::Warn,
                    &format!("Failed to bind port {}: {e}", self.port),
                );
                return Err(Error::IoError(e));
            }
        };

        guard.span().message(
            log::Level::Info,
            &format!("listening on {:?}", listener.local_addr()),
        );

        match axum::serve(listener, app).await {
            Ok(()) => {
                guard.finish_with_outcome(Outcome::Ok);
                Ok(())
            }
            Err(e) => {
                guard.span().message(log::Level::Warn, &e.to_string());
                Err(Error::IoError(e))
            }
        }
    }
}

async fn log_request(State(state): State<AppState>, req: Request, next: Next) -> impl IntoResponse {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let response = next.run(req).await;

    let outcome = if response.status().is_success() {
        Outcome::Ok
    } else {
        Outcome::Failed
    };
    Span::record(
        Arc::clone(&state.reporter),
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

async fn head_blob() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
        "hello world",
    )
}
