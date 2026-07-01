mod blob_paths;
mod blob_store;
mod blob_uploader;
mod bootstrap;
mod digest;
mod name;
mod tag_store;

use crate::{
    blob_store::{BlobError, BlobStore, FileBlobStore},
    blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader},
    digest::DigestError,
    name::{Name, NameParseError},
    tag_store::{FileTagStore, TagStore, TagStoreError},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, head, post},
};
use config::DouglasFolders;
use credentials::create_credentials;
use file_system::{
    EntryKind, FileAppender, FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter,
    Folder, Inspect, UnixFileAppender, UnixFileDeleter, UnixFileReader, UnixFileRenamer,
    UnixFileWriter, UnixFolder, UnixInspect,
};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use serde_json::json;
use std::{path::PathBuf, str::FromStr, sync::Arc};
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

struct RepositoryStore {
    folder: Arc<dyn Folder>,
    repository_root: PathBuf,
}

impl RepositoryStore {
    pub fn new(folder: Arc<dyn Folder>, repository_root: PathBuf) -> Self {
        Self {
            folder,
            repository_root,
        }
    }

    pub fn list(&self) -> Result<Vec<Name>, Error> {
        Ok(self
            .folder
            .entries(&self.repository_root)?
            .iter()
            .filter_map(|entry| {
                if entry.kind != EntryKind::Directory {
                    return None;
                }
                Name::from_str(&entry.name).ok()
            })
            .collect())
    }
}

#[derive(Clone)]
struct BlobState {
    blob_store: Arc<dyn BlobStore>,
    paths: Arc<BlobPaths>,
}

#[derive(Clone)]
struct ManifestState {
    blob_store: Arc<dyn BlobStore>,
    tag_store: Arc<dyn TagStore>,
    paths: Arc<BlobPaths>,
}

#[derive(Clone)]
struct BlobPaths {
    repositories_root: PathBuf,
    folder: Arc<dyn Folder>,
}

impl BlobPaths {
    pub fn new(repositories_root: PathBuf, folder: Arc<dyn Folder>) -> Self {
        Self {
            repositories_root,
            folder,
        }
    }

    pub fn manifest_blob_root(&self, repository_name: &Name) -> Result<PathBuf, Error> {
        let mut result = self.repository_root(repository_name);
        result.push("_manifests");
        result.push("revisions");
        self.folder.create_recursively(&result)?;

        Ok(result)
    }

    pub fn repository_root(&self, name: &Name) -> PathBuf {
        let mut result = self.repositories_root.clone();
        result.push(name.fs_safe());
        result
    }
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    port: u16,
    blob_store: Arc<dyn BlobStore>,
    blob_uploader: Arc<dyn BlobUploader>,
    tag_store: Arc<dyn TagStore>,
    manifest_paths: Arc<BlobPaths>,
    repository_store: Arc<RepositoryStore>,
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
        let file_appender: Arc<dyn FileAppender> = Arc::new(UnixFileAppender::new());

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
                log_path.clone(),
                root_path.clone(),
                repositories_root.clone(),
            )
            .await?;
            Arc::new(BufferedFileReporter::new(log_path))
        };

        let inspect: Arc<dyn Inspect> = Arc::new(UnixInspect::default());
        let blob_store: Arc<dyn BlobStore> = Arc::new(FileBlobStore::new(
            Arc::clone(&folder),
            Arc::clone(&file_writer),
            Arc::clone(&file_reader),
            Arc::clone(&file_renamer),
            Arc::clone(&file_deleter),
            inspect,
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

        let manifest_paths = Arc::new(BlobPaths::new(
            repositories_root.clone(),
            Arc::clone(&folder),
        ));
        let repository_store = Arc::new(RepositoryStore::new(
            Arc::clone(&folder),
            manifest_paths.repositories_root.clone(),
        ));

        Ok(Self {
            reporter,
            port,
            blob_store,
            blob_uploader,
            tag_store,
            manifest_paths,
            repository_store,
        })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();
        let upload_routes = Router::new()
            .route("/v2/{name}/blobs/uploads", post(upload::start))
            .route("/v2/{name}/blobs/uploads/", post(upload::start)) // Docker sends trailing slash
            .route(
                "/v2/{namespace}/{name}/blobs/uploads",
                post(upload::namespaced_start),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads/",
                post(upload::namespaced_start),
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
                get(upload::namespaced_status)
                    .patch(upload::namespaced_write_chunk)
                    .put(upload::namespaced_complete)
                    .delete(upload::namespaced_abort),
            )
            .with_state(Arc::clone(&self.blob_uploader));

        let blob_state = BlobState {
            blob_store: Arc::clone(&self.blob_store),
            paths: Arc::clone(&self.manifest_paths),
        };

        let blob_routes = Router::new()
            .route(
                "/v2/{name}/blobs/{digest}",
                head(blobs::info).get(blobs::blob),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/{digest}",
                head(blobs::namespaced_info).get(blobs::namespaced_blob),
            )
            .with_state(blob_state);

        let manifest_state = ManifestState {
            blob_store: Arc::clone(&self.blob_store),
            tag_store: Arc::clone(&self.tag_store),
            paths: Arc::clone(&self.manifest_paths),
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
                head(manifest::namespaced_info)
                    .get(manifest::namespaced_read)
                    .put(manifest::namespaced_write)
                    .delete(manifest::namespaced_delete),
            )
            .with_state(manifest_state);

        let tags_routes = Router::new()
            .route("/v2/{name}/tags/list", get(tags::list))
            .route(
                "/v2/{namespace}/{name}/tags/list",
                get(tags::namespaced_list),
            )
            .with_state(Arc::clone(&self.tag_store));

        let system_routes = Router::new()
            .route("/v2/_catalog", get(system::catalog))
            .route("/v2/", get(system::v2))
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

    let response = next.run(req).await;

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
    guard.span().message(log::Level::Info, &text);
    guard.finish_with_outcome(outcome);

    response
}

fn error_chain(error: &dyn std::error::Error) -> String {
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

impl From<BlobError> for ServerError {
    fn from(value: BlobError) -> Self {
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

mod error_code {
    pub(crate) const BAD_REQUEST: &str = "BAD_REQUEST";
    pub(crate) const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

    pub(crate) const NAME_INVALID: &str = "NAME_INVALID";
    pub(crate) const NAME_UNKNOWN: &str = "NAME_UNKNOWN";
    pub(crate) const BLOB_UNKNOWN: &str = "BLOB_UNKNOWN";
    pub(crate) const BLOB_UPLOAD_INVALID: &str = "BLOB_UPLOAD_INVALID";
    pub(crate) const BLOB_UPLOAD_UNKNOWN: &str = "BLOB_UPLOAD_UNKNOWN";
    pub(crate) const DIGEST_INVALID: &str = "DIGEST_INVALID";
    pub(crate) const MANIFEST_UNKNOWN: &str = "MANIFEST_UNKNOWN";
    pub(crate) const UNSUPPORTED: &str = "UNSUPPORTED";
}

mod tags {
    use crate::{
        ServerError,
        name::Name,
        tag_store::{TagStore, TagStoreError},
    };
    use axum::{
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use serde_json::{Map, Value};
    use std::{str::FromStr, sync::Arc};

    fn to_tag_error(error: TagStoreError) -> ServerError {
        match error {
            TagStoreError::UnknownRepository(repository) => {
                ServerError::RepositoryUnknown(repository)
            }
            other => ServerError::Internal(Box::new(other)),
        }
    }

    pub(crate) async fn list(
        State(tag_store): State<Arc<dyn TagStore>>,
        Path(name): Path<String>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_tag_list(tag_store, Name::from_str(&name)?).await
    }

    pub(crate) async fn namespaced_list(
        State(tag_store): State<Arc<dyn TagStore>>,
        Path((namespace, name)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_tag_list(tag_store, Name::from_namespaced(&namespace, &name)?).await
    }

    async fn get_tag_list(
        tag_store: Arc<dyn TagStore>,
        name: Name,
    ) -> Result<impl IntoResponse, ServerError> {
        let tags = tag_store.list(&name).map_err(to_tag_error)?;
        let mut map = Map::new();

        map.insert("name".to_string(), Value::String(name.to_string()));
        map.insert(
            "tags".to_string(),
            Value::Array(tags.iter().map(|tag| Value::String(tag.clone())).collect()),
        );

        let result = serde_json::to_string(&map)?;

        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, result.len().to_string()),
                (
                    axum::http::header::CONTENT_TYPE,
                    mime::APPLICATION_JSON.to_string(),
                ),
            ],
            result,
        )
            .into_response())
    }
}

mod manifest {
    use crate::{
        ManifestState, ServerError,
        blob_store::BlobError,
        digest::Digest,
        name::Name,
        tag_store::{TagStore, TagStoreError},
    };
    use axum::{
        body::{Body, Bytes},
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
    };
    use sha2::Sha256;
    use std::{str::FromStr, sync::Arc};
    use tokio_util::io::ReaderStream;

    fn to_manifest_error(error: BlobError) -> ServerError {
        match error {
            BlobError::DigestNotFound(digest) => ServerError::ManifestUnknown(digest),
            other => ServerError::Internal(Box::new(other)),
        }
    }

    pub(crate) async fn info(
        State(state): State<ManifestState>,
        Path((name, reference)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        manifest_info(state, Name::from_str(&name)?, reference).await
    }

    pub(crate) async fn namespaced_info(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        manifest_info(state, Name::from_namespaced(&namespace, &name)?, reference).await
    }

    async fn manifest_info(
        state: ManifestState,
        name: Name,
        reference: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let manifest_blob_root = state.paths.manifest_blob_root(&name)?;

        let digest = get_digest_from_reference(&name, &reference, Arc::clone(&state.tag_store))?;
        let stats = state
            .blob_store
            .stats(&manifest_blob_root, &digest)
            .await
            .map_err(to_manifest_error)?;
        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, stats.size.to_string()),
                (axum::http::header::CONTENT_TYPE, stats.mediatype),
                (
                    axum::http::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
            ],
        )
            .into_response())
    }

    fn get_digest_from_reference(
        name: &Name,
        reference: &str,
        tag_store: Arc<dyn TagStore>,
    ) -> Result<Digest, ServerError> {
        match Digest::from_str(reference) {
            Ok(digest) => Ok(digest),
            Err(_) => match tag_store.read(name, reference) {
                Ok(digest) => Ok(digest),
                Err(TagStoreError::UnknownRepository(_))
                | Err(TagStoreError::UnknwonTag { .. }) => {
                    Err(ServerError::ManifestUnknown(reference.to_string()))
                }
                Err(err) => Err(ServerError::BadRequest(err.to_string())),
            },
        }
    }

    pub(crate) async fn read(
        State(state): State<ManifestState>,
        Path((name, reference)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_manifest(state, Name::from_str(&name)?, reference).await
    }

    pub(crate) async fn namespaced_read(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_manifest(state, Name::from_namespaced(&namespace, &name)?, reference).await
    }

    async fn read_manifest(
        state: ManifestState,
        name: Name,
        reference: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = match Digest::from_str(&reference) {
            Ok(digest) => digest,
            Err(_) => match state.tag_store.read(&name, &reference) {
                Ok(digest) => digest,
                Err(TagStoreError::UnknownRepository(_))
                | Err(TagStoreError::UnknwonTag { .. }) => {
                    return Err(ServerError::ManifestUnknown(reference));
                }
                Err(err) => return Err(ServerError::BadRequest(err.to_string())),
            },
        };
        let manifest_blob_root = state.paths.manifest_blob_root(&name)?;
        let stats = state
            .blob_store
            .stats(&manifest_blob_root, &digest)
            .await
            .map_err(to_manifest_error)?;
        let reader = state
            .blob_store
            .get(&manifest_blob_root, &digest)
            .await
            .map_err(to_manifest_error)?;
        let stream = ReaderStream::new(reader);

        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, stats.size.to_string()),
                (axum::http::header::CONTENT_TYPE, stats.mediatype),
                (
                    axum::http::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
            ],
            Body::from_stream(stream),
        )
            .into_response())
    }

    pub(crate) async fn write(
        State(state): State<ManifestState>,
        Path((name, reference)): Path<(String, String)>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<impl IntoResponse, ServerError> {
        write_manifest(state, Name::from_str(&name)?, reference, headers, body).await
    }

    pub(crate) async fn namespaced_write(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<impl IntoResponse, ServerError> {
        write_manifest(
            state,
            Name::from_namespaced(&namespace, &name)?,
            reference,
            headers,
            body,
        )
        .await
    }

    async fn write_manifest(
        state: ManifestState,
        name: Name,
        reference: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<impl IntoResponse, ServerError> {
        let manifest_blob_root = state.paths.manifest_blob_root(&name)?;
        let media_type = headers
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream");

        let computed = compute_hash(&body)?;
        let digest = match Digest::from_str(&reference) {
            Ok(claimed) => {
                assert_hashes_equal(&computed, &claimed)?;
                let source = Box::new(std::io::Cursor::new(body));
                state
                    .blob_store
                    .save(&manifest_blob_root, &claimed, source, media_type)
                    .await?;
                claimed
            }
            Err(_) => {
                let source = Box::new(std::io::Cursor::new(body));
                state
                    .blob_store
                    .save(&manifest_blob_root, &computed, source, media_type)
                    .await?;

                state.tag_store.write(&name, &reference, &computed)?;
                computed
            }
        };

        Ok((
            StatusCode::CREATED,
            [
                ("Location", format!("/v2/{name}/manifests/{digest}")),
                ("docker-content-digest", digest.to_string()),
            ],
        )
            .into_response())
    }

    fn assert_hashes_equal(computed: &Digest, claimed: &Digest) -> Result<(), ServerError> {
        if computed == claimed {
            Ok(())
        } else {
            Err(ServerError::BadRequest(format!(
                "digest mismatch: claimed {claimed}, computed {computed}"
            )))
        }
    }

    fn compute_hash(body: &Bytes) -> Result<Digest, ServerError> {
        let mut hasher = <Sha256 as sha2::Digest>::new();
        sha2::digest::Update::update(&mut hasher, body);
        let computed = sha2::Digest::finalize(hasher);
        let claimed = Digest::from_bytes(&computed)?;
        Ok(claimed)
    }

    pub(crate) async fn delete(
        State(state): State<ManifestState>,
        Path((name, reference)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        delete_manifest(state, Name::from_str(&name)?, reference).await
    }

    pub(crate) async fn namespaced_delete(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        delete_manifest(state, Name::from_namespaced(&namespace, &name)?, reference).await
    }

    async fn delete_manifest(
        state: ManifestState,
        name: Name,
        reference: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let manifest_blob_root = state.paths.manifest_blob_root(&name)?;
        let digest = Digest::from_str(&reference).map_err(|_| {
            ServerError::MethodNotAllowed(
                "manifest delete requires a digest reference, not a tag".to_string(),
            )
        })?;

        state
            .blob_store
            .delete(&manifest_blob_root, &digest)
            .await
            .map_err(|err| match err {
                BlobError::DigestNotFound(_) => ServerError::ManifestUnknown(digest.to_string()),
                other => ServerError::Internal(Box::new(other)),
            })?;

        Ok(StatusCode::ACCEPTED)
    }
}

mod system {
    use crate::{RepositoryStore, ServerError};
    use axum::{extract::State, http::StatusCode, response::IntoResponse};
    use serde_json::{Map, Value};
    use std::sync::Arc;

    pub(crate) async fn catalog(
        State(repository_store): State<Arc<RepositoryStore>>,
    ) -> Result<impl IntoResponse, ServerError> {
        let names = Value::Array(
            repository_store
                .list()?
                .iter()
                .map(|name| Value::String(name.to_string()))
                .collect(),
        );

        let mut map = Map::new();
        map.insert("repositories".to_string(), names);

        let result = serde_json::to_string(&map)?;

        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, result.len().to_string()),
                (
                    axum::http::header::CONTENT_TYPE,
                    mime::APPLICATION_JSON.to_string(),
                ),
            ],
            result,
        )
            .into_response())
    }

    pub(crate) async fn v2() -> impl IntoResponse {
        (
            StatusCode::OK,
            [("Docker-Distribution-Api-Version", "registry/2.0")],
        )
    }
}

mod upload {
    use crate::{ServerError, blob_uploader::BlobUploader, digest::Digest, name::Name};
    use axum::extract::Query;
    use axum::http::HeaderMap;
    use axum::{
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use http_body::Body as HttpBody;
    use serde::Deserialize;
    use std::{
        io,
        pin::Pin,
        str::FromStr,
        sync::Arc,
        task::{Context, Poll},
    };
    use tokio::io::AsyncRead;
    use tokio::io::ReadBuf;
    use uuid::Uuid;

    pub(crate) async fn start(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path(name): Path<String>,
    ) -> Result<impl IntoResponse, ServerError> {
        start_upload(uploader, Name::from_str(&name)?)
    }

    pub(crate) async fn namespaced_start(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        start_upload(uploader, Name::from_namespaced(&namespace, &name)?)
    }

    fn start_upload(
        uploader: Arc<dyn BlobUploader>,
        name: Name,
    ) -> Result<impl IntoResponse, ServerError> {
        let uuid = uploader.start(&name)?;
        Ok((
            StatusCode::ACCEPTED,
            [("Location", format!("/v2/{name}/blobs/uploads/{uuid}"))],
        ))
    }

    pub(crate) async fn status(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_status(uploader, uuid, Name::from_str(&name)?)
    }

    pub(crate) async fn namespaced_status(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_status(uploader, uuid, Name::from_namespaced(&namespace, &name)?)
    }

    fn get_status(
        uploader: Arc<dyn BlobUploader>,
        uuid: Uuid,
        registry: Name,
    ) -> Result<impl IntoResponse, ServerError> {
        let offset = uploader.status(&registry, uuid)?;
        Ok((
            StatusCode::NO_CONTENT,
            [
                ("Location", format!("/v2/{registry}/blobs/uploads/{uuid}")),
                ("Range", format!("0-{offset}")),
                ("Docker-Upload-UUID", uuid.to_string()),
            ],
        ))
    }

    pub(crate) async fn write_chunk(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
        headers: HeaderMap,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let range_start = parse_range_start(&headers)?;
        write(uploader, Name::from_str(&name)?, uuid, range_start, body).await
    }

    pub(crate) async fn namespaced_write_chunk(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        headers: HeaderMap,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let range_start = parse_range_start(&headers)?;
        write(
            uploader,
            Name::from_namespaced(&namespace, &name)?,
            uuid,
            range_start,
            body,
        )
        .await
    }

    async fn write(
        uploader: Arc<dyn BlobUploader>,
        registry: Name,
        uuid: Uuid,
        range_start: u64,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let offset = uploader
            .write_chunk(&registry, uuid, range_start, body_to_reader(body))
            .await?;
        Ok((
            StatusCode::ACCEPTED,
            [
                ("Location", format!("/v2/{registry}/blobs/uploads/{uuid}")),
                ("Range", format!("0-{offset}")),
                ("Docker-Upload-UUID", uuid.to_string()),
            ],
        ))
    }

    fn parse_range_start(headers: &HeaderMap) -> Result<u64, ServerError> {
        let value = match headers
            .get("content-range")
            .and_then(|header| header.to_str().ok())
        {
            Some(value) => value.to_string(),
            None => return Ok(0),
        };
        value
            .split('-')
            .next()
            .and_then(|start| start.parse().ok())
            .ok_or_else(|| ServerError::BadRequest("malformed Content-Range header".to_string()))
    }

    fn body_to_reader(body: axum::body::Body) -> Box<dyn AsyncRead + Send + Unpin> {
        Box::new(BodyReader {
            body,
            pending: bytes::Bytes::new(),
        })
    }

    struct BodyReader {
        body: axum::body::Body,
        pending: bytes::Bytes,
    }

    impl AsyncRead for BodyReader {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            loop {
                if !this.pending.is_empty() {
                    let read_bytes = this.pending.len().min(buf.remaining());
                    buf.put_slice(&this.pending[..read_bytes]);
                    let _ = this.pending.split_to(read_bytes);
                    return Poll::Ready(Ok(()));
                }
                match Pin::new(&mut this.body).poll_frame(cx) {
                    Poll::Ready(Some(Ok(frame))) => {
                        if let Ok(data) = frame.into_data() {
                            this.pending = data;
                        }
                    }
                    Poll::Ready(Some(Err(err))) => {
                        return Poll::Ready(Err(io::Error::other(err.to_string())));
                    }
                    Poll::Ready(None) => return Poll::Ready(Ok(())),
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    #[derive(Deserialize)]
    pub(crate) struct CompleteParams {
        digest: String,
    }

    pub(crate) async fn complete(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, ServerError> {
        complete_upload(uploader, uuid, params, Name::from_str(&name)?, headers)
    }

    pub(crate) async fn namespaced_complete(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, ServerError> {
        complete_upload(
            uploader,
            uuid,
            params,
            Name::from_namespaced(&namespace, &name)?,
            headers,
        )
    }

    fn complete_upload(
        uploader: Arc<dyn BlobUploader>,
        uuid: Uuid,
        params: CompleteParams,
        registry: Name,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&params.digest)?;
        let media_type = headers
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream");

        uploader.complete(&registry, uuid, &digest, media_type)?;
        Ok((
            StatusCode::CREATED,
            [(
                "Location",
                format!("/v2/{registry}/blobs/{}", params.digest),
            )],
        ))
    }

    pub(crate) async fn abort(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        abort_upload(uploader, Name::from_str(&name)?, uuid)
    }

    pub(crate) async fn namespaced_abort(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        abort_upload(uploader, Name::from_namespaced(&namespace, &name)?, uuid)
    }

    fn abort_upload(
        uploader: Arc<dyn BlobUploader>,
        registry: Name,
        uuid: Uuid,
    ) -> Result<impl IntoResponse, ServerError> {
        uploader.abort(&registry, uuid)?;
        Ok(StatusCode::NO_CONTENT)
    }
}

mod blobs {
    use crate::{BlobState, ServerError, blob_store::BlobError, digest::Digest, name::Name};
    use axum::{
        body::Body,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use std::str::FromStr;
    use tokio_util::io::ReaderStream;

    fn to_blob_error(error: BlobError) -> ServerError {
        match error {
            BlobError::DigestNotFound(_) => ServerError::BlobUnknown(error.to_string()),
            other => ServerError::Internal(Box::new(other)),
        }
    }

    pub(crate) async fn info(
        State(state): State<BlobState>,
        Path((name, raw_digest)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob_info(state, Name::from_str(&name)?, raw_digest).await
    }

    pub(crate) async fn namespaced_info(
        State(state): State<BlobState>,
        Path((namespace, name, raw_digest)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob_info(state, Name::from_namespaced(&namespace, &name)?, raw_digest).await
    }

    async fn read_blob_info(
        state: BlobState,
        name: Name,
        raw_digest: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&raw_digest)?;
        let stats = state
            .blob_store
            .stats(&state.paths.repository_root(&name), &digest)
            .await
            .map_err(to_blob_error)?;
        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, stats.size.to_string()),
                (
                    axum::http::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
            ],
        )
            .into_response())
    }

    pub(crate) async fn blob(
        State(state): State<BlobState>,
        Path((name, raw_digest)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob(state, Name::from_str(&name)?, raw_digest).await
    }

    pub(crate) async fn namespaced_blob(
        State(state): State<BlobState>,
        Path((namespace, name, raw_digest)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob(state, Name::from_namespaced(&namespace, &name)?, raw_digest).await
    }

    async fn read_blob(
        state: BlobState,
        name: Name,
        raw_digest: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&raw_digest)?;
        let blob_root = state.paths.repository_root(&name);
        let stats = state
            .blob_store
            .stats(&blob_root, &digest)
            .await
            .map_err(to_blob_error)?;
        let reader = state
            .blob_store
            .get(&blob_root, &digest)
            .await
            .map_err(to_blob_error)?;
        let stream = ReaderStream::new(reader);

        Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_LENGTH, stats.size.to_string()),
                (
                    axum::http::header::CONTENT_TYPE,
                    mime::APPLICATION_OCTET_STREAM.to_string(),
                ),
                (
                    axum::http::HeaderName::from_static("docker-content-digest"),
                    digest.to_string(),
                ),
            ],
            Body::from_stream(stream),
        )
            .into_response())
    }
}
