mod blob_mounter;
mod blob_paths;
mod blob_store;
mod blob_uploader;
mod blobs;
mod bootstrap;
mod digest;
mod error_code;
mod manifest;
mod proxying_blob_store;
mod repository_store;
mod stream_logging;
mod system;
mod tag_store;
mod tags;
mod token_exchange;
mod upload;

pub use bootstrap::DOUGLAS_RESIN_GROUP;
pub use bootstrap::DOUGLAS_RESIN_USER;
pub use bootstrap::RESIN;
pub use bootstrap::service_definition;

use crate::blob_store::BlobRoot;
use crate::proxying_blob_store::ProxyingBlobStore;
use crate::{
    blob_mounter::{BlobMounter, FileBlobMounter},
    blob_store::{BlobStore, BlobStoreError, FileBlobStore, ResourceKind},
    blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader},
    digest::DigestError,
    repository_store::{FileRepositoryStore, RepositoryStore},
    tag_store::{FileTagStore, TagStore, TagStoreError},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, head, post},
};
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    FileAppender, FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder,
    FolderDeleter, Inspect, Links, Permissions, UnixFileAppender, UnixFileDeleter, UnixFileReader,
    UnixFileRenamer, UnixFileWriter, UnixFolder, UnixFolderDeleter, UnixInspect, UnixLinks,
    UnixPermissions,
};
use futures_util::FutureExt;
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use resin_types::{Name, NameParseError};
use serde_json::json;
use std::path::Path;
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
    #[error("Failed to bootstrap: {0:?}")]
    FailedBoostrap(Vec<String>),
}

#[derive(Clone)]
struct ErrorDetail(String);

enum ServerError {
    BlobUnknown(String),
    ManifestUnknown(String),
    BadRequest(String),
    MethodNotAllowed(String),
    Internal(Box<dyn std::error::Error + Send + Sync>),
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },
    RepositoryUnknown(String),
    InvalidName(String),
    SeedlingNotRegistered(String),
    SeedlingReserved(String),
}

impl From<serde_json::Error> for ServerError {
    fn from(err: serde_json::Error) -> ServerError {
        ServerError::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

impl From<NameParseError> for ServerError {
    fn from(value: NameParseError) -> Self {
        match value {
            NameParseError::CannotBeEmpty => {
                ServerError::InvalidName("Invalid name: cannot be empty".to_string())
            }
            NameParseError::TooLong => {
                ServerError::InvalidName("Invalid name: too long".to_string())
            }
            NameParseError::InvalidName => ServerError::InvalidName("Invalid name".to_string()),
        }
    }
}

impl From<FileSystemError> for ServerError {
    fn from(err: FileSystemError) -> ServerError {
        ServerError::Internal(Box::new(err))
    }
}

impl From<seedling_registration_client::Error> for ServerError {
    fn from(err: seedling_registration_client::Error) -> ServerError {
        ServerError::Internal(Box::new(err))
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, message) = match self {
            ServerError::BlobUnknown(detail) => {
                (StatusCode::NOT_FOUND, error_code::BLOB_UNKNOWN, detail)
            }
            ServerError::ManifestUnknown(detail) => {
                (StatusCode::NOT_FOUND, error_code::MANIFEST_UNKNOWN, detail)
            }
            ServerError::BadRequest(detail) => {
                (StatusCode::BAD_REQUEST, error_code::BAD_REQUEST, detail)
            }
            ServerError::MethodNotAllowed(detail) => (
                StatusCode::METHOD_NOT_ALLOWED,
                error_code::UNSUPPORTED,
                detail,
            ),
            ServerError::Internal(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_code::INTERNAL_ERROR,
                error_chain(error.as_ref()),
            ),
            ServerError::ParseError {
                line,
                column,
                message,
            } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_code::INTERNAL_ERROR,
                format!("error parsing JSON on line {line}:{column}: '{message}'"),
            ),
            ServerError::RepositoryUnknown(repository) => (
                StatusCode::NOT_FOUND,
                error_code::NAME_UNKNOWN,
                format!("unknown repository {repository}"),
            ),
            ServerError::InvalidName(description) => (
                StatusCode::BAD_REQUEST,
                error_code::NAME_INVALID,
                description,
            ),
            ServerError::SeedlingNotRegistered(name) => (
                StatusCode::BAD_REQUEST,
                error_code::SEEDLING_NOT_REGISTERED,
                format!("seedling '{name}' is not registered"),
            ),
            ServerError::SeedlingReserved(name) => (
                StatusCode::BAD_REQUEST,
                error_code::SEEDLING_RESERVED,
                format!("seedling '{name}' is reserved and cannot be pushed to directly"),
            ),
        };
        let mut response = (
            status,
            Json(json!({ "errors": [{ "code": code, "message": message }] })),
        )
            .into_response();
        response.extensions_mut().insert(ErrorDetail(message));
        response
    }
}

async fn reject_namespaced() -> ServerError {
    ServerError::BadRequest("namespaced seedlings are not supported".to_string())
}

#[derive(Clone)]
struct UploadState {
    blob_uploader: Arc<dyn BlobUploader>,
    blob_mounter: Arc<dyn BlobMounter>,
}

#[derive(Clone)]
struct BlobState {
    blob_store: Arc<dyn BlobStore>,
    reporter: Arc<dyn Reporter>,
}

#[derive(Clone)]
struct ManifestState {
    blob_store: Arc<dyn BlobStore>,
    tag_store: Arc<dyn TagStore>,
    reporter: Arc<dyn Reporter>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
    reconcile_trigger_client: Arc<dyn reconcile_trigger_client::Client>,
}

struct LocalBlobRoot {
    repositories_root: PathBuf,
    folder: Arc<dyn Folder>,
}

impl LocalBlobRoot {
    pub fn new(repositories_root: &Path, folder: Arc<dyn Folder>) -> Self {
        Self {
            repositories_root: repositories_root.to_path_buf(),
            folder,
        }
    }

    fn create_repository_root_path(name: &Name, repositories_root: &Path) -> PathBuf {
        let mut result = repositories_root.to_path_buf();
        result.push(name.fs_safe());
        result
    }
}

impl BlobRoot for LocalBlobRoot {
    fn get(&self, name: &Name, resource_kind: ResourceKind) -> Result<PathBuf, FileSystemError> {
        let mut result = Self::create_repository_root_path(name, &self.repositories_root);

        if resource_kind == ResourceKind::Manifest {
            result.push("_manifests");
            result.push("revisions");
            self.folder.create_recursively(&result)?;
        }

        Ok(result)
    }
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    port: u16,
    blob_store: Arc<dyn BlobStore>,
    blob_uploader: Arc<dyn BlobUploader>,
    blob_mounter: Arc<dyn BlobMounter>,
    tag_store: Arc<dyn TagStore>,
    repository_store: Arc<dyn RepositoryStore>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
    reconcile_trigger_client: Arc<dyn reconcile_trigger_client::Client>,
}

impl Server {
    pub async fn build(reporting_fd: Option<i32>, port: u16) -> Result<Self, Error> {
        let os: Arc<dyn Os> = Arc::new(Unix::new());
        let credentials = create_credentials(Arc::clone(&os));
        let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
        let douglas_folders = DouglasFolders::new();
        let file_renamer: Arc<dyn FileRenamer> = Arc::new(UnixFileRenamer::new());
        let file_deleter: Arc<dyn FileDeleter> = Arc::new(UnixFileDeleter::new());
        let folder_deleter: Arc<dyn FolderDeleter> = Arc::new(UnixFolderDeleter::new());
        let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
        let file_writer: Arc<dyn FileWriter> = Arc::new(UnixFileWriter::new());
        let file_appender: Arc<dyn FileAppender> = Arc::new(UnixFileAppender::new());
        let links: Arc<dyn Links> = Arc::new(UnixLinks::new());
        let permissions: Arc<dyn Permissions> = Arc::new(UnixPermissions::new());

        let (root_path, log_path) = if reporting_fd.is_none() {
            let mut root = std::env::temp_dir();
            root.push("douglas-resin-dbg");
            (root, douglas_folders.service_log_file(RESIN))
        } else {
            (
                douglas_folders.seedling_root(RESIN),
                douglas_folders.service_log_file(RESIN),
            )
        };

        let mut repositories_root = root_path.clone();
        repositories_root.push("repositories");

        let reporter: Arc<dyn Reporter> = if reporting_fd.is_none() {
            let tui = TuiReporter::start()?;
            for dir in [&root_path, &repositories_root] {
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
                &*permissions,
                &douglas_folders,
            )
            .await?;
            Arc::new(BufferedFileReporter::new(log_path))
        };

        let inspect: Arc<dyn Inspect> = Arc::new(UnixInspect::default());
        let local_blob_store: Arc<dyn BlobStore> = Arc::new(FileBlobStore::new(
            Arc::new(LocalBlobRoot::new(&repositories_root, Arc::clone(&folder))),
            Arc::clone(&folder),
            Arc::clone(&file_writer),
            Arc::clone(&file_reader),
            Arc::clone(&file_renamer),
            Arc::clone(&file_deleter),
            Arc::clone(&inspect),
        ));

        let blob_store: Arc<dyn BlobStore> = Arc::new(ProxyingBlobStore::new(
            Arc::clone(&reporter),
            local_blob_store,
        ));

        let blob_uploader: Arc<dyn BlobUploader> = Arc::new(FileBlobUploader::new(
            repositories_root.clone(),
            Arc::clone(&folder),
            Arc::clone(&file_writer),
            file_appender,
            Arc::clone(&file_renamer),
            Arc::clone(&file_deleter),
        ));

        let tag_store: Arc<dyn TagStore> = Arc::new(FileTagStore::new(
            repositories_root.clone(),
            Arc::clone(&folder),
            Arc::clone(&file_reader),
            Arc::clone(&file_writer),
            Arc::clone(&file_deleter),
        ));

        let repository_store: Arc<dyn RepositoryStore> = Arc::new(FileRepositoryStore::new(
            repositories_root.clone(),
            Arc::clone(&folder),
            Arc::clone(&folder_deleter),
        ));

        let blob_mounter = Arc::new(FileBlobMounter::new(
            Arc::clone(&repository_store),
            Arc::clone(&folder),
            Arc::clone(&inspect),
            Arc::clone(&file_deleter),
            Arc::clone(&links),
            repositories_root.clone(),
        ));

        let seedling_registration_client: Arc<dyn seedling_registration_client::Client> = Arc::new(
            seedling_registration_client::UdsClient::new(Arc::clone(&reporter), &douglas_folders),
        );

        let reconcile_trigger_client: Arc<dyn reconcile_trigger_client::Client> = Arc::new(
            reconcile_trigger_client::UdsClient::new(Arc::clone(&reporter), &douglas_folders),
        );

        Ok(Self {
            reporter,
            port,
            blob_store,
            blob_uploader,
            blob_mounter,
            tag_store,
            repository_store,
            seedling_registration_client,
            reconcile_trigger_client,
        })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let upload_state = UploadState {
            blob_uploader: Arc::clone(&self.blob_uploader),
            blob_mounter: Arc::clone(&self.blob_mounter),
        };

        let upload_routes = Router::new()
            .route("/v2/{name}/blobs/uploads", post(upload::start))
            .route("/v2/{name}/blobs/uploads/", post(upload::start)) // Docker sends trailing slash
            .route(
                "/v2/{namespace}/{name}/blobs/uploads",
                post(reject_namespaced),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads/",
                post(reject_namespaced),
            )
            .route(
                "/v2/{name}/blobs/uploads/{uuid}",
                get(upload::status)
                    .patch(upload::write_chunk)
                    .put(upload::complete)
                    .delete(upload::abort),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads/{uuid}",
                get(reject_namespaced)
                    .patch(reject_namespaced)
                    .put(reject_namespaced)
                    .delete(reject_namespaced),
            )
            .with_state(upload_state);

        let blob_state = BlobState {
            blob_store: Arc::clone(&self.blob_store),
            reporter: Arc::clone(&self.reporter),
        };

        let blob_routes = Router::new()
            .route(
                "/v2/{name}/blobs/{digest}",
                head(blobs::info).get(blobs::blob).delete(blobs::delete),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/{digest}",
                head(blobs::info_namespaced)
                    .get(blobs::blob_namespaced)
                    .delete(reject_namespaced),
            )
            .with_state(blob_state);

        let manifest_state = ManifestState {
            blob_store: Arc::clone(&self.blob_store),
            tag_store: Arc::clone(&self.tag_store),
            reporter: Arc::clone(&self.reporter),
            seedling_registration_client: Arc::clone(&self.seedling_registration_client),
            reconcile_trigger_client: Arc::clone(&self.reconcile_trigger_client),
        };

        let manifest_routes = Router::new()
            .route(
                "/v2/{name}/manifests/{ref}",
                head(manifest::info)
                    .get(manifest::read)
                    .put(manifest::write)
                    .delete(manifest::delete),
            )
            .route(
                "/v2/{namespace}/{name}/manifests/{ref}",
                head(manifest::info_namespaced)
                    .get(manifest::read_namespaced)
                    .put(reject_namespaced)
                    .delete(reject_namespaced),
            )
            .with_state(manifest_state);

        let tags_routes = Router::new()
            .route("/v2/{name}/tags/list", get(tags::list))
            .route("/v2/{namespace}/{name}/tags/list", get(reject_namespaced))
            .with_state(Arc::clone(&self.tag_store));

        let system_routes = Router::new()
            .route("/v2/_catalog", get(system::catalog))
            .route("/v2/", get(system::v2))
            .route("/v2/{name}/", delete(system::delete_repository))
            .route("/v2/{namespace}/{name}/", delete(reject_namespaced))
            .with_state(Arc::clone(&self.repository_store));

        let app = Router::new()
            .merge(upload_routes)
            .merge(blob_routes)
            .merge(manifest_routes)
            .merge(tags_routes)
            .merge(system_routes)
            .layer(middleware::from_fn_with_state(
                Arc::clone(&self.reporter),
                log_request,
            ))
            .layer(DefaultBodyLimit::disable());

        let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", self.port)).await
        {
            Ok(listener) => listener,
            Err(err) => {
                guard.span().message(
                    log::Level::Warn,
                    &format!("Failed to bind port {}: {err}", self.port),
                );
                return Err(Error::IoError(err));
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
            Err(err) => {
                guard.span().message(log::Level::Warn, &err.to_string());
                Err(Error::IoError(err))
            }
        }
    }
}

async fn log_request(
    State(reporter): State<Arc<dyn Reporter>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let guard = Span::new(
        Arc::clone(&reporter),
        &format!("{method} {uri}"),
        ScopeKind::Task,
    )
    .start_guard();

    let mut text = format!("{method} {uri} HTTP/1.1");
    for (name, value) in req.headers() {
        if let Ok(val) = value.to_str() {
            text.push_str(&format!("\n{name}: {val}"));
        }
    }
    guard.span().message(log::Level::Info, &text);

    // Deliberately run `next.run(req)` in place (not `tokio::spawn`ed) — HTTP/1.1
    // request bodies are streamed cooperatively with the connection's own task,
    // and moving the service future onto a different task desyncs body delivery
    // (observed as every chunked upload failing with "error reading a body from
    // connection"). `catch_unwind` still lets us turn a handler panic into a
    // clean 500 instead of taking down the connection.
    let response = match std::panic::AssertUnwindSafe(next.run(req))
        .catch_unwind()
        .await
    {
        Ok(response) => response,
        Err(payload) => {
            let details = panic_message(payload);
            guard.span().message(
                log::Level::Warn,
                &format!("request handler panicked: {details}"),
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "errors": [{ "code": error_code::INTERNAL_ERROR, "message": details }] })),
            )
                .into_response()
        }
    };

    let outcome = if response.status().is_success() {
        Outcome::Ok
    } else {
        Outcome::Failed
    };

    let mut text = format!("HTTP/1.1 {}", response.status());
    for (name, value) in response.headers() {
        if let Ok(val) = value.to_str() {
            text.push_str(&format!("\n{name}: {val}"));
        }
    }
    if let Some(ErrorDetail(detail)) = response.extensions().get::<ErrorDetail>() {
        text.push_str(&format!("\n\n{detail}"));
    }
    guard.span().message(log::Level::Info, &text);
    guard.finish_with_outcome(outcome);

    response
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain = format!("{chain}: {cause}");
        source = cause.source();
    }
    chain
}

impl From<DigestError> for ServerError {
    fn from(value: DigestError) -> Self {
        ServerError::BadRequest(value.to_string())
    }
}

impl From<TagStoreError> for ServerError {
    fn from(value: TagStoreError) -> Self {
        ServerError::BadRequest(value.to_string())
    }
}

impl From<Error> for ServerError {
    fn from(value: Error) -> Self {
        ServerError::Internal(Box::new(value))
    }
}

impl From<BlobStoreError> for ServerError {
    fn from(value: BlobStoreError) -> Self {
        ServerError::Internal(Box::new(value))
    }
}

impl From<BlobUploaderError> for ServerError {
    fn from(err: BlobUploaderError) -> Self {
        match err {
            BlobUploaderError::InvalidRespository(repository) => {
                ServerError::RepositoryUnknown(repository)
            }
            BlobUploaderError::UnknownUuid { uuid, name } => ServerError::BlobUnknown(format!(
                "upload session '{uuid}' for registry {name} not found"
            )),
            BlobUploaderError::DigestMismatch { claimed, computed } => ServerError::BadRequest(
                format!("digest mismatch: claimed {claimed}, computed {computed}"),
            ),
            BlobUploaderError::RangeMismatch { expected, received } => ServerError::BadRequest(
                format!("range mismatch: expected offset {expected}, got {received}"),
            ),
            BlobUploaderError::NameParseError(name_error) => ServerError::from(name_error),
            other => ServerError::Internal(Box::new(other)),
        }
    }
}

impl IntoResponse for BlobUploaderError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, message) = match &self {
            BlobUploaderError::InvalidRespository(repository) => (
                StatusCode::BAD_REQUEST,
                error_code::NAME_UNKNOWN,
                format!("repository '{repository}' is not registered"),
            ),
            BlobUploaderError::UnknownUuid { uuid, name } => (
                StatusCode::NOT_FOUND,
                error_code::BLOB_UPLOAD_UNKNOWN,
                format!("upload session '{uuid}' for repository {name} not found",),
            ),
            BlobUploaderError::DigestMismatch { claimed, computed } => (
                StatusCode::BAD_REQUEST,
                error_code::DIGEST_INVALID,
                format!("digest mismatch: claimed {claimed}, computed {computed}",),
            ),
            BlobUploaderError::RangeMismatch { expected, received } => (
                StatusCode::BAD_REQUEST,
                error_code::BLOB_UPLOAD_INVALID,
                format!("range mismatch: expected offset {expected}, got {received}"),
            ),
            BlobUploaderError::NameParseError(name_error) => (
                StatusCode::BAD_REQUEST,
                error_code::NAME_INVALID,
                name_error.to_string(),
            ),
            BlobUploaderError::FileSystemError(_)
            | BlobUploaderError::HashFailure
            | BlobUploaderError::DigestError(_)
            | BlobUploaderError::NetworkError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_code::UNSUPPORTED,
                self.to_string(),
            ),
        };
        (
            status,
            Json(json!({ "errors": [{ "code": code, "message": message }] })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod panic_message_tests {
    use super::panic_message;

    #[test]
    fn test_panic_message_should_extract_a_static_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");

        assert_eq!(panic_message(payload), "boom");
    }

    #[test]
    fn test_panic_message_should_extract_a_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());

        assert_eq!(panic_message(payload), "boom");
    }

    #[test]
    fn test_panic_message_should_fall_back_for_an_unknown_payload_type() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42);

        assert_eq!(panic_message(payload), "unknown panic payload");
    }
}
