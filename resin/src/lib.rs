mod authorize;
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
    Json, Router, ServiceExt,
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
use heartbeat::{HeartbeatWriter, LocalHeartbeatWriter};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use resin_types::{NameParseError, Repository, RepositoryError};
use serde_json::json;
use std::path::Path;
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;
use tower::{Layer, util::MapRequestLayer};

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

impl From<RepositoryError> for ServerError {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::NotLocal(repository) => ServerError::MethodNotAllowed(format!(
                "{repository} is not available: upstream repositories are not supported yet"
            )),
            other => ServerError::InvalidName(other.to_string()),
        }
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

#[derive(Clone)]
struct UploadState {
    blob_uploader: Arc<dyn BlobUploader>,
    blob_mounter: Arc<dyn BlobMounter>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
}

#[derive(Clone)]
struct BlobState {
    blob_store: Arc<dyn BlobStore>,
    reporter: Arc<dyn Reporter>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
}

#[derive(Clone)]
struct ManifestState {
    blob_store: Arc<dyn BlobStore>,
    tag_store: Arc<dyn TagStore>,
    reporter: Arc<dyn Reporter>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
    reconcile_trigger_client: Arc<dyn reconcile_trigger_client::Client>,
}

#[derive(Clone)]
struct SystemState {
    repository_store: Arc<dyn RepositoryStore>,
    seedling_registration_client: Arc<dyn seedling_registration_client::Client>,
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

    fn create_repository_root_path(repository: &Repository, repositories_root: &Path) -> PathBuf {
        let mut result = repositories_root.to_path_buf();
        result.push(repository.storage_path());
        result
    }
}

impl BlobRoot for LocalBlobRoot {
    fn get(
        &self,
        repository: &Repository,
        resource_kind: ResourceKind,
    ) -> Result<PathBuf, FileSystemError> {
        let mut result = Self::create_repository_root_path(repository, &self.repositories_root);

        if resource_kind == ResourceKind::Manifest {
            result.push("_manifests");
            result.push("revisions");
            self.folder.create_recursively(&result)?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod local_blob_root_tests {
    use super::{BlobRoot, LocalBlobRoot, ResourceKind};
    use file_system::MockFolder;
    use resin_types::Repository;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    fn test_should_root_a_local_repository_under_the_local_subtree() {
        let blob_root =
            LocalBlobRoot::new(&PathBuf::from("/repositories"), Arc::new(MockFolder::new()));
        let repository: Repository = "hello-world".parse().unwrap();

        let result = blob_root.get(&repository, ResourceKind::Blob).unwrap();

        assert_eq!(result, PathBuf::from("/repositories/local/hello-world"));
    }

    #[test]
    fn test_should_root_an_upstream_repository_under_its_host() {
        let blob_root =
            LocalBlobRoot::new(&PathBuf::from("/repositories"), Arc::new(MockFolder::new()));
        let repository: Repository = "ghcr.io/foo/bar".parse().unwrap();

        let result = blob_root.get(&repository, ResourceKind::Blob).unwrap();

        assert_eq!(
            result,
            PathBuf::from("/repositories/upstream/ghcr.io/foo%2Fbar")
        );
    }

    #[test]
    fn test_should_create_the_revisions_directory_for_a_manifest() {
        let mut folder = MockFolder::new();
        folder
            .expect_create_recursively()
            .withf(|path| {
                path == std::path::Path::new("/repositories/local/hello-world/_manifests/revisions")
            })
            .returning(|path| Ok(path.to_path_buf()));

        let blob_root = LocalBlobRoot::new(&PathBuf::from("/repositories"), Arc::new(folder));
        let repository: Repository = "hello-world".parse().unwrap();

        let result = blob_root.get(&repository, ResourceKind::Manifest).unwrap();

        assert_eq!(
            result,
            PathBuf::from("/repositories/local/hello-world/_manifests/revisions")
        );
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
    heartbeat_writer: Box<dyn HeartbeatWriter>,
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

        let heartbeat_writer = Box::new(LocalHeartbeatWriter::new(
            Arc::clone(&file_writer),
            &douglas_folders.service_heartbeat_file(RESIN),
        ));

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
            heartbeat_writer,
        })
    }

    fn upload_routes(&self) -> Router {
        let upload_state = UploadState {
            blob_uploader: Arc::clone(&self.blob_uploader),
            blob_mounter: Arc::clone(&self.blob_mounter),
            seedling_registration_client: Arc::clone(&self.seedling_registration_client),
        };

        Router::new()
            .route("/v2/{repository}/blobs/uploads", post(upload::start))
            .route("/v2/{repository}/blobs/uploads/", post(upload::start)) // Docker sends trailing slash
            .route(
                "/v2/{repository}/blobs/uploads/{uuid}",
                get(upload::status)
                    .patch(upload::write_chunk)
                    .put(upload::complete)
                    .delete(upload::abort),
            )
            .with_state(upload_state)
    }

    fn blob_routes(&self) -> Router {
        let blob_state = BlobState {
            blob_store: Arc::clone(&self.blob_store),
            reporter: Arc::clone(&self.reporter),
            seedling_registration_client: Arc::clone(&self.seedling_registration_client),
        };

        Router::new()
            .route(
                "/v2/{repository}/blobs/{digest}",
                head(blobs::info).get(blobs::blob).delete(blobs::delete),
            )
            .with_state(blob_state)
    }

    fn manifest_routes(&self) -> Router {
        let manifest_state = ManifestState {
            blob_store: Arc::clone(&self.blob_store),
            tag_store: Arc::clone(&self.tag_store),
            reporter: Arc::clone(&self.reporter),
            seedling_registration_client: Arc::clone(&self.seedling_registration_client),
            reconcile_trigger_client: Arc::clone(&self.reconcile_trigger_client),
        };

        Router::new()
            .route(
                "/v2/{repository}/manifests/{ref}",
                head(manifest::info)
                    .get(manifest::read)
                    .put(manifest::write)
                    .delete(manifest::delete),
            )
            .with_state(manifest_state)
    }

    fn tags_routes(&self) -> Router {
        Router::new()
            .route("/v2/{repository}/tags/list", get(tags::list))
            .with_state(Arc::clone(&self.tag_store))
    }

    fn system_routes(&self) -> Router {
        let system_state = SystemState {
            repository_store: Arc::clone(&self.repository_store),
            seedling_registration_client: Arc::clone(&self.seedling_registration_client),
        };

        Router::new()
            .route("/v2/_catalog", get(system::catalog))
            .route("/v2/", get(system::v2))
            .route("/v2/{repository}/", delete(system::delete_repository))
            .with_state(system_state)
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let app = Router::new()
            .merge(self.upload_routes())
            .merge(self.blob_routes())
            .merge(self.manifest_routes())
            .merge(self.tags_routes())
            .merge(self.system_routes())
            .layer(middleware::from_fn_with_state(
                Arc::clone(&self.reporter),
                log_request,
            ))
            .layer(DefaultBodyLimit::disable());
        let app = MapRequestLayer::new(fold_repository_segment).layer(app);

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

        let make_service = app.into_make_service();
        let serve_result = axum::serve(listener, make_service);

        tokio::select! {
            result = serve_result => match result {
                Ok(()) => {
                    guard.finish_with_outcome(Outcome::Ok);
                    Ok(())
                }
                Err(err) => {
                    guard.span().message(log::Level::Warn, &err.to_string());
                    Err(Error::IoError(err))
                }
            },
            () = self.heartbeat_loop() => unreachable!("heartbeat loop never returns"),
        }
    }

    async fn heartbeat_loop(&self) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        loop {
            interval.tick().await;

            let span = Span::new(
                Arc::clone(&self.reporter),
                "Updating heartbeat",
                ScopeKind::Task,
            );

            if let Err(err) = self.heartbeat_writer.write() {
                span.message(log::Level::Warn, &format!("Heartbeat write failed: {err}"));
            }
        }
    }
}

fn tail_length(segments: &[&str]) -> Option<usize> {
    match segments {
        [.., "blobs", "uploads", _] => Some(3),
        [.., "blobs", "uploads"] => Some(2),
        [.., "blobs", _] => Some(2),
        [.., "manifests", _] => Some(2),
        [.., "tags", "list"] => Some(2),
        _ => None,
    }
}

fn fold_path(path: &str) -> String {
    let Some(rest) = path.strip_prefix("/v2/") else {
        return path.to_string();
    };
    let trailing_slash = rest.ends_with('/');
    let trimmed = rest.strip_suffix('/').unwrap_or(rest);
    if trimmed.is_empty() {
        return path.to_string();
    }

    let segments: Vec<&str> = trimmed.split('/').collect();
    let tail = tail_length(&segments).or(if trailing_slash { Some(0) } else { None });
    let Some(tail) = tail else {
        return path.to_string();
    };

    let name_length = segments.len() - tail;
    if name_length < 1 {
        return path.to_string();
    }

    let folded_name = segments[..name_length].join("%2F");
    let tail_segments = &segments[name_length..];

    if tail_segments.is_empty() {
        format!("/v2/{folded_name}/")
    } else {
        let tail_path = tail_segments.join("/");
        if trailing_slash {
            format!("/v2/{folded_name}/{tail_path}/")
        } else {
            format!("/v2/{folded_name}/{tail_path}")
        }
    }
}

fn fold_repository_segment(mut request: Request) -> Request {
    let folded = fold_path(request.uri().path());
    let rewritten = match request.uri().query() {
        Some(query) => format!("{folded}?{query}"),
        None => folded,
    };
    if let Ok(uri) = rewritten.parse() {
        *request.uri_mut() = uri;
    }
    request
}

#[cfg(test)]
mod fold_path_tests {
    use super::fold_path;

    #[test]
    fn test_should_leave_a_single_segment_repository_unchanged() {
        assert_eq!(
            fold_path("/v2/hello-world/blobs/uploads"),
            "/v2/hello-world/blobs/uploads"
        );
    }

    #[test]
    fn test_should_fold_a_namespaced_local_repository() {
        assert_eq!(
            fold_path("/v2/openbao/openbao/manifests/2.4.3"),
            "/v2/openbao%2Fopenbao/manifests/2.4.3"
        );
    }

    #[test]
    fn test_should_fold_a_multi_segment_upstream_repository() {
        assert_eq!(
            fold_path("/v2/ghcr.io/foo/bar/manifests/1.0"),
            "/v2/ghcr.io%2Ffoo%2Fbar/manifests/1.0"
        );
    }

    #[test]
    fn test_should_fold_blobs_uploads_with_a_trailing_slash() {
        assert_eq!(
            fold_path("/v2/openbao/openbao/blobs/uploads/"),
            "/v2/openbao%2Fopenbao/blobs/uploads/"
        );
    }

    #[test]
    fn test_should_fold_a_blob_upload_continuation() {
        assert_eq!(
            fold_path("/v2/openbao/openbao/blobs/uploads/some-uuid"),
            "/v2/openbao%2Fopenbao/blobs/uploads/some-uuid"
        );
    }

    #[test]
    fn test_should_fold_a_bare_repository_delete_path() {
        assert_eq!(fold_path("/v2/openbao/openbao/"), "/v2/openbao%2Fopenbao/");
    }

    #[test]
    fn test_should_leave_the_catalog_route_unchanged() {
        assert_eq!(fold_path("/v2/_catalog"), "/v2/_catalog");
    }

    #[test]
    fn test_should_leave_the_ping_route_unchanged() {
        assert_eq!(fold_path("/v2/"), "/v2/");
    }

    #[test]
    fn test_should_leave_a_non_v2_path_unchanged() {
        assert_eq!(fold_path("/healthz"), "/healthz");
    }

    #[test]
    fn test_should_fold_tags_list() {
        assert_eq!(
            fold_path("/v2/openbao/openbao/tags/list"),
            "/v2/openbao%2Fopenbao/tags/list"
        );
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
