mod blob_paths;
mod blob_store;
mod blob_uploader;
mod bootstrap;
mod digest;
mod tag_store;

use crate::{
    blob_store::{BlobError, BlobStore, FileBlobStore},
    blob_uploader::{BlobUploader, BlobUploaderError, FileBlobUploader},
    digest::DigestError,
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
    FileAppender, FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder,
    Inspect, UnixFileAppender, UnixFileDeleter, UnixFileReader, UnixFileRenamer, UnixFileWriter,
    UnixFolder, UnixInspect,
};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
use serde_json::json;
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

#[derive(Clone)]
struct ErrorDetail(String);

enum ServerError {
    BlobUnknown(String),
    ManifestUnknown(String),
    BadRequest(String),
    MethodNotAllowed(String),
    Internal(Box<dyn std::error::Error + Send + Sync>),
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
            ServerError::MethodNotAllowed(detail) => {
                (StatusCode::METHOD_NOT_ALLOWED, error_code::UNSUPPORTED, detail)
            }
            ServerError::Internal(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_code::INTERNAL_ERROR,
                error_chain(error.as_ref()),
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
struct ManifestState {
    blob_store: Arc<dyn BlobStore>,
    tag_store: Arc<dyn TagStore>,
    paths: Arc<BlobPaths>,
}

#[derive(Clone)]
struct BlobPaths {
    blob_root: PathBuf,
    repositories_root: PathBuf,
    folder: Arc<dyn Folder>,
}

impl BlobPaths {
    pub fn new(blob_root: PathBuf, repositories_root: PathBuf, folder: Arc<dyn Folder>) -> Self {
        Self {
            blob_root,
            repositories_root,
            folder,
        }
    }

    pub fn manifest_blob_root(&self, repository_name: &str) -> Result<PathBuf, Error> {
        let mut result = self.repositories_root.clone();
        result.push(repository_name);
        result.push("_manifests");
        result.push("revisions");
        self.folder.create_recursively(&result)?;

        Ok(result)
    }

    pub fn blob_root(&self) -> PathBuf {
        self.blob_root.clone()
    }
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    port: u16,
    blob_store: Arc<dyn BlobStore>,
    blob_uploader: Arc<dyn BlobUploader>,
    tag_store: Arc<dyn TagStore>,
    manifest_paths: Arc<BlobPaths>,
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

        let mut blob_root = root_path.clone();
        blob_root.push("blobs");

        let mut tmp_root = root_path.clone();
        tmp_root.push("tmp");

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
            tmp_root,
            blob_root.clone(),
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

        Ok(Self {
            reporter,
            port,
            blob_store,
            blob_uploader,
            tag_store,
            manifest_paths: Arc::new(BlobPaths::new(
                blob_root.clone(),
                repositories_root.clone(),
                Arc::clone(&folder),
            )),
        })
    }

    pub async fn start(&self) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let general = Router::new().route("/v2/", get(v2));

        let read_state = ManifestState {
            blob_store: Arc::clone(&self.blob_store),
            tag_store: Arc::clone(&self.tag_store),
            paths: Arc::clone(&self.manifest_paths),
        };

        let upload_routes = Router::new()
            .route("/v2/{name}/blobs/uploads", post(upload::start))
            .route("/v2/{name}/blobs/uploads/", post(upload::start)) // Docker sends trailing slash
            .route(
                "/v2/{name}/blobs/uploads/{uuid}",
                get(upload::status)
                    .patch(upload::write_chunk)
                    .put(upload::complete)
                    .delete(upload::abort),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads",
                post(upload::namespaced_start),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads/",
                post(upload::namespaced_start),
            )
            .route(
                "/v2/{namespace}/{name}/blobs/uploads/{uuid}",
                get(upload::namespaced_status)
                    .patch(upload::namespaced_write_chunk)
                    .put(upload::namespaced_complete)
                    .delete(upload::namespaced_ns),
            )
            .with_state(Arc::clone(&self.blob_uploader));

        let read_routes = Router::new()
            .route(
                "/v2/{name}/blobs/{digest}",
                head(read::blob_info).get(read::blob),
            )
            .route(
                "/v2/{name}/manifests/{ref}",
                head(manifest::info)
                    .get(manifest::read)
                    .put(manifest::write)
                .delete(manifest::delete),

            )
            .route(
                "/v2/{namespace}/{name}/blobs/{digest}",
                head(read::blob_info_ns).get(read::namespaced_blob),
            )
            .route(
                "/v2/{namespace}/{name}/manifests/{ref}",
                head(manifest::namespaced_info)
                    .get(manifest::namespaced_read)
                    .put(manifest::namespaced_write)
                    .delete(manifest::namespaced_delete),
            )
            .with_state(read_state);

        let app = Router::new()
            .merge(general)
            .merge(upload_routes)
            .merge(read_routes)
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

impl IntoResponse for BlobUploaderError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, message) = match &self {
            BlobUploaderError::InvalidRespository(r) => (
                StatusCode::BAD_REQUEST,
                error_code::NAME_UNKNOWN,
                format!("repository '{}' is not registered", r),
            ),
            BlobUploaderError::UnknownUuid(u) => (
                StatusCode::NOT_FOUND,
                error_code::BLOB_UPLOAD_UNKNOWN,
                format!("upload session '{}' not found", u),
            ),
            BlobUploaderError::DigestMismatch { claimed, computed } => (
                StatusCode::BAD_REQUEST,
                error_code::DIGEST_INVALID,
                format!(
                    "digest mismatch: claimed {}, computed {}",
                    claimed, computed
                ),
            ),
            BlobUploaderError::RangeMismatch { expected, received } => (
                StatusCode::BAD_REQUEST,
                error_code::BLOB_UPLOAD_INVALID,
                format!("range mismatch: expected offset {expected}, got {received}"),
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

    pub(crate) const NAME_UNKNOWN: &str = "NAME_UNKNOWN";
    pub(crate) const BLOB_UNKNOWN: &str = "BLOB_UNKNOWN";
    pub(crate) const BLOB_UPLOAD_INVALID: &str = "BLOB_UPLOAD_INVALID";
    pub(crate) const BLOB_UPLOAD_UNKNOWN: &str = "BLOB_UPLOAD_UNKNOWN";
    pub(crate) const DIGEST_INVALID: &str = "DIGEST_INVALID";
    pub(crate) const MANIFEST_UNKNOWN: &str = "MANIFEST_UNKNOWN";
    pub(crate) const UNSUPPORTED: &str = "UNSUPPORTED";
}

async fn v2() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
    )
}

mod manifest {
    use crate::{
        ManifestState, ServerError,
        blob_store::BlobError,
        digest::Digest,
        tag_store::{self, TagStore, TagStoreError},
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
        manifest_info(state, name, reference).await
    }

    pub(crate) async fn namespaced_info(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        manifest_info(state, format!("{namespace}/{name}"), reference).await
    }

    async fn manifest_info(
        state: ManifestState,
        name: String,
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
        name: &str,
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
        read_manifest(state, name, reference).await
    }

    pub(crate) async fn namespaced_read(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_manifest(state, format!("{namespace}/{name}"), reference).await
    }

    async fn read_manifest(
        state: ManifestState,
        name: String,
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
        write_manifest(state, name, reference, headers, body).await
    }

    pub(crate) async fn namespaced_write(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<impl IntoResponse, ServerError> {
        write_manifest(
            state,
            format!("{namespace}/{name}"),
            reference,
            headers,
            body,
        )
        .await
    }

    async fn write_manifest(
        state: ManifestState,
        name: String,
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
        delete_manifest(state, name, reference).await
    }

    pub(crate) async fn namespaced_delete(
        State(state): State<ManifestState>,
        Path((namespace, name, reference)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        delete_manifest(state, format!("{namespace}/{name}"), reference).await
    }

    async fn delete_manifest(
        state: ManifestState,
        name: String,
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
                crate::blob_store::BlobError::DigestNotFound(_) => {
                    ServerError::ManifestUnknown(digest.to_string())
                }
                other => ServerError::Internal(Box::new(other)),
            })?;

        Ok(StatusCode::ACCEPTED)
    }
}

mod read {
    use crate::{ManifestState, ServerError, blob_store::BlobError, digest::Digest};
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

    pub(crate) async fn blob_info(
        State(state): State<ManifestState>,
        Path((_name, raw_digest)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob_info(state, raw_digest).await
    }

    pub(crate) async fn blob_info_ns(
        State(state): State<ManifestState>,
        Path((_namespace, _name, raw_digest)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob_info(state, raw_digest).await
    }

    async fn read_blob_info(
        state: ManifestState,
        raw_digest: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&raw_digest)?;
        let stats = state
            .blob_store
            .stats(&state.paths.blob_root(), &digest)
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
        State(state): State<ManifestState>,
        Path((_name, raw_digest)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob(state, raw_digest).await
    }

    pub(crate) async fn namespaced_blob(
        State(state): State<ManifestState>,
        Path((_namespace, _name, raw_digest)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        read_blob(state, raw_digest).await
    }

    async fn read_blob(
        state: ManifestState,
        raw_digest: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&raw_digest)?;
        let stats = state
            .blob_store
            .stats(&state.paths.blob_root(), &digest)
            .await
            .map_err(to_blob_error)?;
        let reader = state
            .blob_store
            .get(&state.paths.blob_root(), &digest)
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

mod upload {
    use crate::blob_uploader::BlobUploaderError;
    use crate::{ServerError, blob_uploader::BlobUploader, digest::Digest};
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
    use tokio::io::{AsyncRead, ReadBuf};
    use uuid::Uuid;

    #[derive(Deserialize)]
    pub(crate) struct CompleteParams {
        digest: String,
    }

    pub(crate) async fn start(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path(name): Path<String>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        start_upload(uploader, name)
    }

    pub(crate) async fn namespaced_start(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let name = format!("{namespace}/{name}");
        start_upload(uploader, name)
    }

    fn start_upload(
        uploader: Arc<dyn BlobUploader>,
        name: String,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let uuid = uploader.start(&name)?;
        Ok((
            StatusCode::ACCEPTED,
            [("Location", format!("/v2/{name}/blobs/uploads/{uuid}"))],
        ))
    }

    pub(crate) async fn status(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        get_status(uploader, uuid, name)
    }

    pub(crate) async fn namespaced_status(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let name = format!("{namespace}/{name}");
        get_status(uploader, uuid, name)
    }

    fn get_status(
        uploader: Arc<dyn BlobUploader>,
        uuid: Uuid,
        name: String,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let offset = uploader.status(uuid)?;
        Ok((
            StatusCode::NO_CONTENT,
            [
                ("Location", format!("/v2/{name}/blobs/uploads/{uuid}")),
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
    ) -> Result<impl IntoResponse, axum::response::Response> {
        let range_start = parse_range_start(&headers).map_err(IntoResponse::into_response)?;
        write(uploader, name, uuid, range_start, body).await
    }

    pub(crate) async fn namespaced_write_chunk(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        headers: HeaderMap,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, axum::response::Response> {
        let range_start = parse_range_start(&headers).map_err(IntoResponse::into_response)?;
        let name = format!("{namespace}/{name}");
        write(uploader, name, uuid, range_start, body).await
    }

    async fn write(
        uploader: Arc<dyn BlobUploader>,
        name: String,
        uuid: Uuid,
        range_start: u64,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, axum::response::Response> {
        let offset = uploader
            .write_chunk(uuid, range_start, body_to_reader(body))
            .await
            .map_err(IntoResponse::into_response)?;
        Ok((
            StatusCode::ACCEPTED,
            [
                ("Location", format!("/v2/{name}/blobs/uploads/{uuid}")),
                ("Range", format!("0-{offset}")),
                ("Docker-Upload-UUID", uuid.to_string()),
            ],
        ))
    }

    fn parse_range_start(headers: &HeaderMap) -> Result<u64, ServerError> {
        let value = headers
            .get("content-range")
            .and_then(|header| header.to_str().ok())
            .ok_or_else(|| {
                ServerError::BadRequest("malformed or missing Content-Range header".to_string())
            })?;
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

    pub(crate) async fn complete(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((name, uuid)): Path<(String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        complete_upload(uploader, uuid, params, name, headers)
    }

    pub(crate) async fn namespaced_complete(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let name = format!("{namespace}/{name}");
        complete_upload(uploader, uuid, params, name, headers)
    }

    fn complete_upload(
        uploader: Arc<dyn BlobUploader>,
        uuid: Uuid,
        params: CompleteParams,
        name: String,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        let digest = Digest::from_str(&params.digest)?;
        let media_type = headers
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream");

        uploader.complete(uuid, &digest, media_type)?;
        Ok((
            StatusCode::CREATED,
            [("Location", format!("/v2/{name}/blobs/{}", params.digest))],
        ))
    }

    pub(crate) async fn abort(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((_name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        abort_upload(uploader, uuid)
    }

    pub(crate) async fn namespaced_ns(
        State(uploader): State<Arc<dyn BlobUploader>>,
        Path((_namespace, _name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        abort_upload(uploader, uuid)
    }

    fn abort_upload(
        uploader: Arc<dyn BlobUploader>,
        uuid: Uuid,
    ) -> Result<impl IntoResponse, BlobUploaderError> {
        uploader.abort(uuid)?;
        Ok(StatusCode::NO_CONTENT)
    }
}
