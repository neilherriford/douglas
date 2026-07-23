mod blob_mounter;
mod blob_paths;
mod blob_store;
mod blob_uploader;
mod bootstrap;
mod digest;
mod name;
mod proxying_blob_store;
mod repository_store;
mod tag_store;
mod token_exchange;

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
    name::{Name, NameParseError},
    repository_store::{FileRepositoryStore, RepositoryStore},
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
    Inspect, Links, Permissions, UnixFileAppender, UnixFileDeleter, UnixFileReader,
    UnixFileRenamer, UnixFileWriter, UnixFolder, UnixInspect, UnixLinks, UnixPermissions,
};
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span, TuiReporter};
use os::{Os, Unix};
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

impl From<FileSystemError> for ServerError {
    fn from(err: FileSystemError) -> ServerError {
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
}

mod stream_logging {
    use log::{Reporter, ScopeKind, Span};
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, ReadBuf};

    pub(crate) struct LoggingReader {
        inner: Box<dyn AsyncRead + Send + Unpin>,
        reporter: Arc<dyn Reporter>,
        context: String,
    }

    impl LoggingReader {
        pub(crate) fn new(
            inner: Box<dyn AsyncRead + Send + Unpin>,
            reporter: Arc<dyn Reporter>,
            context: String,
        ) -> Self {
            Self {
                inner,
                reporter,
                context,
            }
        }
    }

    impl AsyncRead for LoggingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let result = Pin::new(&mut this.inner).poll_read(cx, buf);

            if let Poll::Ready(Err(err)) = &result {
                let span = Span::new(Arc::clone(&this.reporter), "Stream error", ScopeKind::Task);
                span.message(
                    log::Level::Warn,
                    &format!(
                        "{}: streaming failed after response headers were already sent: {err}",
                        this.context
                    ),
                );
            }

            result
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use log::Event;
        use tokio::io::AsyncReadExt;

        struct NullReporter;

        impl Reporter for NullReporter {
            fn emit(&self, _event: Event) {}
        }

        fn test_reporter() -> Arc<dyn Reporter> {
            Arc::new(NullReporter)
        }

        struct ErroringReader;

        impl AsyncRead for ErroringReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Err(std::io::Error::other("boom")))
            }
        }

        #[tokio::test]
        async fn test_logging_reader_should_pass_bytes_through_unchanged() {
            let inner: Box<dyn AsyncRead + Send + Unpin> =
                Box::new(std::io::Cursor::new(b"hello".to_vec()));
            let mut reader = LoggingReader::new(inner, test_reporter(), "ctx".to_string());

            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();

            assert_eq!(buf, b"hello");
        }

        #[tokio::test]
        async fn test_logging_reader_should_propagate_errors_after_logging() {
            let inner: Box<dyn AsyncRead + Send + Unpin> = Box::new(ErroringReader);
            let mut reader = LoggingReader::new(inner, test_reporter(), "ctx".to_string());

            let mut buf = Vec::new();
            let result = reader.read_to_end(&mut buf).await;

            assert!(result.is_err());
        }
    }
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
        let links: Arc<dyn Links> = Arc::new(UnixLinks::new());
        let permissions: Arc<dyn Permissions> = Arc::new(UnixPermissions::new());

        let (root_path, log_path) = if reporting_fd.is_none() {
            let mut root = std::env::temp_dir();
            root.push("douglas-resin-dbg");
            (root, douglas_folders.service_log_file(RESIN))
        } else {
            (
                douglas_folders.service_root(RESIN),
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
        ));

        let blob_mounter = Arc::new(FileBlobMounter::new(
            Arc::clone(&repository_store),
            Arc::clone(&folder),
            Arc::clone(&inspect),
            Arc::clone(&file_deleter),
            Arc::clone(&links),
            repositories_root.clone(),
        ));

        Ok(Self {
            reporter,
            port,
            blob_store,
            blob_uploader,
            blob_mounter,
            tag_store,
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

        let upload_state = UploadState {
            blob_uploader: Arc::clone(&self.blob_uploader),
            blob_mounter: Arc::clone(&self.blob_mounter),
        };

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
                head(blobs::namespaced_info)
                    .get(blobs::namespaced_blob)
                    .delete(blobs::namespaced_delete),
            )
            .with_state(blob_state);

        let manifest_state = ManifestState {
            blob_store: Arc::clone(&self.blob_store),
            tag_store: Arc::clone(&self.tag_store),
            reporter: Arc::clone(&self.reporter),
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

    let response = match tokio::spawn(next.run(req)).await {
        Ok(response) => response,
        Err(join_err) => {
            let details = if join_err.is_panic() {
                panic_message(join_err.into_panic())
            } else {
                "request handling was cancelled".to_string()
            };
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

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain = format!("{chain}: {cause}");
        source = cause.source();
    }
    chain
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
        extract::{Path, Query, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use serde::Deserialize;
    use serde_json::{Map, Value};
    use std::{str::FromStr, sync::Arc};

    #[derive(Deserialize)]
    pub(crate) struct TagListParams {
        n: Option<usize>,
        last: Option<String>,
    }

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
        Query(params): Query<TagListParams>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_tag_list(tag_store, Name::from_str(&name)?, params).await
    }

    pub(crate) async fn namespaced_list(
        State(tag_store): State<Arc<dyn TagStore>>,
        Path((namespace, name)): Path<(String, String)>,
        Query(params): Query<TagListParams>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_tag_list(tag_store, Name::from_namespaced(&namespace, &name)?, params).await
    }

    fn paginate_tags<'a>(
        tags: &'a [String],
        last: Option<&str>,
        index: Option<usize>,
    ) -> (&'a [String], bool) {
        let start = match last {
            Some(last) => tags.partition_point(|tag| tag.as_str() <= last),
            None => 0,
        };
        let remaining = &tags[start..];

        match index {
            Some(index) if index < remaining.len() => (&remaining[..index], true),
            _ => (remaining, false),
        }
    }

    async fn get_tag_list(
        tag_store: Arc<dyn TagStore>,
        name: Name,
        params: TagListParams,
    ) -> Result<impl IntoResponse, ServerError> {
        let all_tags = tag_store.list(&name).map_err(to_tag_error)?;
        let (page, has_more) = paginate_tags(&all_tags, params.last.as_deref(), params.n);

        let mut map = Map::new();
        map.insert("name".to_string(), Value::String(name.to_string()));
        map.insert(
            "tags".to_string(),
            Value::Array(page.iter().map(|tag| Value::String(tag.clone())).collect()),
        );

        let result = serde_json::to_string(&map)?;

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            result.len().to_string().parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            mime::APPLICATION_JSON.to_string().parse().unwrap(),
        );

        if has_more && let (Some(n), Some(last_tag)) = (params.n, page.last()) {
            headers.insert(
                axum::http::header::LINK,
                format!("</v2/{name}/tags/list?n={n}&last={last_tag}>; rel=\"next\"")
                    .parse()
                    .unwrap(),
            );
        }

        Ok((StatusCode::OK, headers, result).into_response())
    }

    #[cfg(test)]
    mod tests {
        mod paginate_tags {
            use crate::tags::paginate_tags;

            fn tags(values: &[&str]) -> Vec<String> {
                values.iter().map(|value| value.to_string()).collect()
            }

            #[test]
            fn test_should_return_everything_when_no_params_given() {
                let tags = tags(&["a", "b", "c"]);

                let (page, has_more) = paginate_tags(&tags, None, None);

                assert_eq!(page, &tags[..]);
                assert!(!has_more);
            }

            #[test]
            fn test_should_truncate_to_n_and_signal_more_when_n_is_smaller() {
                let tags = tags(&["a", "b", "c"]);

                let (page, has_more) = paginate_tags(&tags, None, Some(2));

                assert_eq!(page, &["a".to_string(), "b".to_string()]);
                assert!(has_more);
            }

            #[test]
            fn test_should_return_everything_when_n_is_larger_than_remaining() {
                let tags = tags(&["a", "b", "c"]);

                let (page, has_more) = paginate_tags(&tags, None, Some(10));

                assert_eq!(page, &tags[..]);
                assert!(!has_more);
            }

            #[test]
            fn test_should_resume_after_last() {
                let tags = tags(&["a", "b", "c", "d"]);

                let (page, has_more) = paginate_tags(&tags, Some("b"), None);

                assert_eq!(page, &["c".to_string(), "d".to_string()]);
                assert!(!has_more);
            }

            #[test]
            fn test_should_resume_after_last_and_page_by_n() {
                let tags = tags(&["a", "b", "c", "d"]);

                let (page, has_more) = paginate_tags(&tags, Some("a"), Some(1));

                assert_eq!(page, &["b".to_string()]);
                assert!(has_more);
            }

            #[test]
            fn test_should_return_empty_when_last_is_the_final_tag() {
                let tags = tags(&["a", "b", "c"]);

                let (page, has_more) = paginate_tags(&tags, Some("c"), None);

                assert!(page.is_empty());
                assert!(!has_more);
            }

            #[test]
            fn test_should_resume_correctly_when_last_no_longer_exists() {
                let tags = tags(&["a", "c", "d"]);

                let (page, has_more) = paginate_tags(&tags, Some("b"), None);

                assert_eq!(page, &["c".to_string(), "d".to_string()]);
                assert!(!has_more);
            }
        }
    }
}

mod manifest {
    use crate::{
        ManifestState, ServerError,
        blob_store::{BlobStore, BlobStoreError, ResourceKind},
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
    use log::{Reporter, ScopeKind, Span};
    use sha2::Sha256;
    use std::{str::FromStr, sync::Arc};
    use tokio_util::io::ReaderStream;

    fn to_manifest_error(error: BlobStoreError) -> ServerError {
        match error {
            BlobStoreError::DigestNotFound(digest) => ServerError::ManifestUnknown(digest),
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
        // let manifest_blob_root = state.paths.manifest_blob_root(&name)?;

        let digest = get_digest_from_reference(
            Arc::clone(&state.blob_store),
            &name,
            &reference,
            Arc::clone(&state.tag_store),
        )
        .await?;
        let stats = state
            .blob_store
            .stats(&name, &digest, ResourceKind::Manifest)
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

    async fn get_digest_from_reference(
        blob_store: Arc<dyn BlobStore>,
        name: &Name,
        reference: &str,
        tag_store: Arc<dyn TagStore>,
    ) -> Result<Digest, ServerError> {
        if let Ok(digest) = Digest::from_str(reference) {
            return Ok(digest);
        }

        match tag_store.read(name, reference) {
            Ok(digest) => Ok(digest),
            Err(TagStoreError::UnknownRepository(_)) | Err(TagStoreError::UnknwonTag { .. }) => {
                blob_store
                    .resolve_reference(name, reference, ResourceKind::Manifest)
                    .await
                    .map_err(|err| match err {
                        BlobStoreError::DigestNotFound(_) => {
                            ServerError::ManifestUnknown(reference.to_string())
                        }
                        other => ServerError::Internal(Box::new(other)),
                    })
            }
            Err(err) => Err(ServerError::BadRequest(err.to_string())),
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
        let digest = get_digest_from_reference(
            Arc::clone(&state.blob_store),
            &name,
            &reference,
            Arc::clone(&state.tag_store),
        )
        .await?;
        let stats = state
            .blob_store
            .stats(&name, &digest, ResourceKind::Manifest)
            .await
            .map_err(to_manifest_error)?;
        let reader = state
            .blob_store
            .get(&name, &digest, ResourceKind::Manifest)
            .await
            .map_err(to_manifest_error)?;
        let reader = crate::stream_logging::LoggingReader::new(
            reader,
            Arc::clone(&state.reporter),
            format!("manifest {name} {digest}"),
        );
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
        // let manifest_blob_root = state.paths.manifest_blob_root(&name)?;
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
                    .save(&name, &claimed, source, media_type, ResourceKind::Manifest)
                    .await?;
                claimed
            }
            Err(_) => {
                let source = Box::new(std::io::Cursor::new(body));
                state
                    .blob_store
                    .save(&name, &computed, source, media_type, ResourceKind::Manifest)
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
        // let manifest_blob_root = state.paths.manifest_blob_root(&name)?;
        let digest = Digest::from_str(&reference).map_err(|_| {
            ServerError::MethodNotAllowed(
                "manifest delete requires a digest reference, not a tag".to_string(),
            )
        })?;

        state
            .blob_store
            .delete(&name, &digest, ResourceKind::Manifest)
            .await
            .map_err(|err| match err {
                BlobStoreError::DigestNotFound(_) => {
                    ServerError::ManifestUnknown(digest.to_string())
                }
                other => ServerError::Internal(Box::new(other)),
            })?;

        remove_tags_pointing_to(state.tag_store.as_ref(), &state.reporter, &name, &digest);

        Ok(StatusCode::ACCEPTED)
    }

    fn remove_tags_pointing_to(
        tag_store: &dyn TagStore,
        reporter: &Arc<dyn Reporter>,
        name: &Name,
        digest: &Digest,
    ) {
        let span = Span::new(Arc::clone(reporter), "Tag cleanup", ScopeKind::Task);

        let tags = match tag_store.list(name) {
            Ok(tags) => tags,
            Err(err) => {
                span.message(
                    log::Level::Warn,
                    &format!("failed to list tags for {name} while cleaning up after deleting {digest}: {err}"),
                );
                return;
            }
        };

        for tag in tags {
            let points_at_deleted_digest = tag_store
                .read(name, &tag)
                .is_ok_and(|resolved| resolved == *digest);

            if !points_at_deleted_digest {
                continue;
            }

            if let Err(err) = tag_store.delete(name, &tag) {
                span.message(
                    log::Level::Warn,
                    &format!("failed to delete stale tag {name}:{tag} pointing at {digest}: {err}"),
                );
            }
        }
    }

    #[cfg(test)]
    mod tests {
        mod get_digest_from_reference {
            use crate::{
                ServerError,
                blob_store::{BlobStoreError, MockBlobStore},
                digest::Digest,
                manifest::get_digest_from_reference,
                name::Name,
                tag_store::{MockTagStore, TagStoreError},
            };
            use std::{str::FromStr, sync::Arc};

            #[tokio::test]
            async fn test_should_return_digest_directly_when_reference_is_a_digest() {
                let name = Name::from_str("foo").unwrap();
                let tag_store = MockTagStore::new();
                let blob_store = MockBlobStore::new();
                let sha = "ff".repeat(32);

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    &format!("sha256:{sha}"),
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Ok(digest) if digest.hex() == sha));
            }

            #[tokio::test]
            async fn test_should_resolve_tag_via_tag_store_when_reference_is_not_a_digest() {
                let name = Name::from_str("foo").unwrap();
                let mut tag_store = MockTagStore::new();
                let blob_store = MockBlobStore::new();
                let sha = "ff".repeat(32);
                let expected = Digest(format!("sha256:{sha}"));

                tag_store
                    .expect_read()
                    .withf(|_, tag| tag == "latest")
                    .returning(move |_, _| Ok(Digest(format!("sha256:{sha}"))));

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    "latest",
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Ok(digest) if digest == expected));
            }

            #[tokio::test]
            async fn test_should_resolve_remotely_when_tag_is_unknown_locally() {
                let name = Name::from_str("foo").unwrap();
                let mut tag_store = MockTagStore::new();
                let mut blob_store = MockBlobStore::new();
                let sha = "ff".repeat(32);
                let expected = Digest(format!("sha256:{sha}"));

                tag_store.expect_read().returning(|_, _| {
                    Err(TagStoreError::UnknwonTag {
                        repository: "foo".to_string(),
                        tag: "latest".to_string(),
                    })
                });
                blob_store
                    .expect_resolve_reference()
                    .withf(|_, reference, _| reference == "latest")
                    .returning(move |_, _, _| Ok(Digest(format!("sha256:{sha}"))));

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    "latest",
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Ok(digest) if digest == expected));
            }

            #[tokio::test]
            async fn test_should_return_manifest_unknown_when_repository_is_unknown_and_remote_resolution_fails()
             {
                let name = Name::from_str("foo").unwrap();
                let mut tag_store = MockTagStore::new();
                let mut blob_store = MockBlobStore::new();

                tag_store
                    .expect_read()
                    .returning(|_, _| Err(TagStoreError::UnknownRepository("foo".to_string())));
                blob_store
                    .expect_resolve_reference()
                    .returning(|_, reference, _| {
                        Err(BlobStoreError::DigestNotFound(reference.to_string()))
                    });

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    "latest",
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Err(ServerError::ManifestUnknown(_))));
            }

            #[tokio::test]
            async fn test_should_return_manifest_unknown_when_tag_is_unknown_and_remote_resolution_fails()
             {
                let name = Name::from_str("foo").unwrap();
                let mut tag_store = MockTagStore::new();
                let mut blob_store = MockBlobStore::new();

                tag_store.expect_read().returning(|_, _| {
                    Err(TagStoreError::UnknwonTag {
                        repository: "foo".to_string(),
                        tag: "latest".to_string(),
                    })
                });
                blob_store
                    .expect_resolve_reference()
                    .returning(|_, reference, _| {
                        Err(BlobStoreError::DigestNotFound(reference.to_string()))
                    });

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    "latest",
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Err(ServerError::ManifestUnknown(_))));
            }

            #[tokio::test]
            async fn test_should_return_bad_request_for_other_tag_store_errors() {
                let name = Name::from_str("foo").unwrap();
                let mut tag_store = MockTagStore::new();
                let blob_store = MockBlobStore::new();

                tag_store.expect_read().returning(|_, _| {
                    Err(TagStoreError::DigestError(
                        crate::digest::DigestError::InvalidDigest,
                    ))
                });

                let actual = get_digest_from_reference(
                    Arc::new(blob_store),
                    &name,
                    "latest",
                    Arc::new(tag_store),
                )
                .await;

                assert!(matches!(actual, Err(ServerError::BadRequest(_))));
            }
        }

        mod compute_hash {
            use crate::manifest::compute_hash;
            use axum::body::Bytes;

            #[test]
            fn test_should_compute_sha256_of_body() {
                let body = Bytes::from_static(b"Lorem ipsum dolor sit amet");
                let expected = "16aba5393ad72c0041f5600ad3c2c52ec437a2f0c7fc08fadfc3c0fe9641d7a3";

                let actual = compute_hash(&body);

                assert!(matches!(actual, Ok(digest) if digest.hex() == expected));
            }

            #[test]
            fn test_should_compute_different_hashes_for_different_bodies() {
                let (Ok(first), Ok(second)) = (
                    compute_hash(&Bytes::from_static(b"foo")),
                    compute_hash(&Bytes::from_static(b"bar")),
                ) else {
                    panic!("expected both hashes to compute successfully");
                };

                assert_ne!(first, second);
            }
        }

        mod assert_hashes_equal {
            use crate::{digest::Digest, manifest::assert_hashes_equal};

            #[test]
            fn test_should_succeed_when_hashes_match() {
                let sha = "ff".repeat(32);
                let computed = Digest(format!("sha256:{sha}"));
                let claimed = Digest(format!("sha256:{sha}"));

                assert!(assert_hashes_equal(&computed, &claimed).is_ok());
            }

            #[test]
            fn test_should_fail_when_hashes_differ() {
                let computed = Digest(format!("sha256:{}", "ff".repeat(32)));
                let claimed = Digest(format!("sha256:{}", "00".repeat(32)));

                assert!(assert_hashes_equal(&computed, &claimed).is_err());
            }
        }

        mod remove_tags_pointing_to {
            use crate::{
                digest::Digest, manifest::remove_tags_pointing_to, name::Name,
                tag_store::MockTagStore,
            };
            use log::{Event, Reporter};
            use std::{str::FromStr, sync::Arc};

            struct NullReporter;

            impl Reporter for NullReporter {
                fn emit(&self, _event: Event) {}
            }

            fn test_reporter() -> Arc<dyn Reporter> {
                Arc::new(NullReporter)
            }

            #[test]
            fn test_should_delete_only_tags_that_resolve_to_the_digest() {
                let name = Name::from_str("foo").unwrap();
                let deleted = Digest(format!("sha256:{}", "ff".repeat(32)));
                let other = Digest(format!("sha256:{}", "00".repeat(32)));
                let mut tag_store = MockTagStore::new();

                tag_store
                    .expect_list()
                    .returning(|_| Ok(vec!["latest".to_string(), "stable".to_string()]));
                tag_store
                    .expect_read()
                    .withf(|_, tag| tag == "latest")
                    .returning({
                        let deleted = deleted.clone();
                        move |_, _| Ok(deleted.clone())
                    });
                tag_store
                    .expect_read()
                    .withf(|_, tag| tag == "stable")
                    .returning({
                        let other = other.clone();
                        move |_, _| Ok(other.clone())
                    });
                tag_store
                    .expect_delete()
                    .withf(|_, tag| tag == "latest")
                    .returning(|_, _| Ok(true));

                remove_tags_pointing_to(&tag_store, &test_reporter(), &name, &deleted);
            }

            #[test]
            fn test_should_do_nothing_when_repository_has_no_tags() {
                let name = Name::from_str("foo").unwrap();
                let digest = Digest(format!("sha256:{}", "ff".repeat(32)));
                let mut tag_store = MockTagStore::new();

                tag_store.expect_list().returning(|_| Ok(Vec::new()));

                remove_tags_pointing_to(&tag_store, &test_reporter(), &name, &digest);
            }

            #[test]
            fn test_should_do_nothing_when_listing_tags_fails() {
                let name = Name::from_str("foo").unwrap();
                let digest = Digest(format!("sha256:{}", "ff".repeat(32)));
                let mut tag_store = MockTagStore::new();

                tag_store.expect_list().returning(|_| {
                    Err(crate::tag_store::TagStoreError::UnknownRepository(
                        "foo".to_string(),
                    ))
                });

                remove_tags_pointing_to(&tag_store, &test_reporter(), &name, &digest);
            }

            #[test]
            fn test_should_continue_without_panicking_when_a_delete_fails() {
                let name = Name::from_str("foo").unwrap();
                let deleted = Digest(format!("sha256:{}", "ff".repeat(32)));
                let mut tag_store = MockTagStore::new();

                tag_store
                    .expect_list()
                    .returning(|_| Ok(vec!["latest".to_string()]));
                tag_store.expect_read().returning({
                    let deleted = deleted.clone();
                    move |_, _| Ok(deleted.clone())
                });
                tag_store.expect_delete().returning(|_, _| {
                    Err(crate::tag_store::TagStoreError::UnknownRepository(
                        "foo".to_string(),
                    ))
                });

                remove_tags_pointing_to(&tag_store, &test_reporter(), &name, &deleted);
            }
        }
    }
}

mod system {
    use crate::{RepositoryStore, ServerError};
    use axum::{extract::State, http::StatusCode, response::IntoResponse};
    use serde_json::{Map, Value};
    use std::sync::Arc;

    pub(crate) async fn catalog(
        State(repository_store): State<Arc<dyn RepositoryStore>>,
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
    use crate::UploadState;
    use crate::{ServerError, digest::Digest, name::Name};
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
        task::{Context, Poll},
    };
    use tokio::io::AsyncRead;
    use tokio::io::ReadBuf;
    use uuid::Uuid;

    #[derive(Deserialize)]
    pub(crate) struct StartParams {
        mount: Option<String>,
        from: Option<String>,
    }

    pub(crate) async fn start(
        State(state): State<UploadState>,
        Path(name): Path<String>,
        Query(params): Query<StartParams>,
    ) -> Result<impl IntoResponse, ServerError> {
        start_upload(state, Name::from_str(&name)?, params)
    }

    pub(crate) async fn namespaced_start(
        State(state): State<UploadState>,
        Path((namespace, name)): Path<(String, String)>,
        Query(params): Query<StartParams>,
    ) -> Result<impl IntoResponse, ServerError> {
        start_upload(state, Name::from_namespaced(&namespace, &name)?, params)
    }

    fn start_upload(
        state: UploadState,
        name: Name,
        params: StartParams,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = params.mount.map(|raw| Digest::from_str(&raw)).transpose()?;
        let source_registry = params.from.and_then(|raw| Name::from_str(&raw).ok());

        if let Some(digest) = digest {
            let mounted =
                state
                    .blob_mounter
                    .mount_blob(source_registry.as_ref(), &digest, &name)?;
            if mounted {
                return Ok((
                    StatusCode::CREATED,
                    [
                        ("Location", format!("/v2/{name}/blobs/{digest}")),
                        ("docker-content-digest", digest.to_string()),
                    ],
                )
                    .into_response());
            }
        }

        let uuid = state.blob_uploader.start(&name)?;
        Ok((
            StatusCode::ACCEPTED,
            [("Location", format!("/v2/{name}/blobs/uploads/{uuid}"))],
        )
            .into_response())
    }

    pub(crate) async fn status(
        State(state): State<UploadState>,
        Path((name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_status(state, uuid, Name::from_str(&name)?)
    }

    pub(crate) async fn namespaced_status(
        State(state): State<UploadState>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        get_status(state, uuid, Name::from_namespaced(&namespace, &name)?)
    }

    fn get_status(
        state: UploadState,
        uuid: Uuid,
        registry: Name,
    ) -> Result<impl IntoResponse, ServerError> {
        let offset = state.blob_uploader.status(&registry, uuid)?;
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
        State(state): State<UploadState>,
        Path((name, uuid)): Path<(String, Uuid)>,
        headers: HeaderMap,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let range_start = parse_range_start(&headers)?;
        write(state, Name::from_str(&name)?, uuid, range_start, body).await
    }

    pub(crate) async fn namespaced_write_chunk(
        State(state): State<UploadState>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        headers: HeaderMap,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let range_start = parse_range_start(&headers)?;
        write(
            state,
            Name::from_namespaced(&namespace, &name)?,
            uuid,
            range_start,
            body,
        )
        .await
    }

    async fn write(
        state: UploadState,
        registry: Name,
        uuid: Uuid,
        range_start: u64,
        body: axum::body::Body,
    ) -> Result<impl IntoResponse, ServerError> {
        let offset = state
            .blob_uploader
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
        State(state): State<UploadState>,
        Path((name, uuid)): Path<(String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, ServerError> {
        complete_upload(state, uuid, params, Name::from_str(&name)?, headers)
    }

    pub(crate) async fn namespaced_complete(
        State(state): State<UploadState>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
        Query(params): Query<CompleteParams>,
        headers: HeaderMap,
    ) -> Result<impl IntoResponse, ServerError> {
        complete_upload(
            state,
            uuid,
            params,
            Name::from_namespaced(&namespace, &name)?,
            headers,
        )
    }

    fn complete_upload(
        state: UploadState,
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

        state
            .blob_uploader
            .complete(&registry, uuid, &digest, media_type)?;
        Ok((
            StatusCode::CREATED,
            [(
                "Location",
                format!("/v2/{registry}/blobs/{}", params.digest),
            )],
        ))
    }

    pub(crate) async fn abort(
        State(state): State<UploadState>,
        Path((name, uuid)): Path<(String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        abort_upload(state, Name::from_str(&name)?, uuid)
    }

    pub(crate) async fn namespaced_abort(
        State(state): State<UploadState>,
        Path((namespace, name, uuid)): Path<(String, String, Uuid)>,
    ) -> Result<impl IntoResponse, ServerError> {
        abort_upload(state, Name::from_namespaced(&namespace, &name)?, uuid)
    }

    fn abort_upload(
        state: UploadState,
        registry: Name,
        uuid: Uuid,
    ) -> Result<impl IntoResponse, ServerError> {
        state.blob_uploader.abort(&registry, uuid)?;
        Ok(StatusCode::NO_CONTENT)
    }

    #[cfg(test)]
    mod tests {
        mod parse_range_start {
            use crate::{ServerError, upload::parse_range_start};
            use axum::http::HeaderMap;

            #[test]
            fn test_should_return_zero_when_header_is_absent() {
                let headers = HeaderMap::new();

                let actual = parse_range_start(&headers);

                assert!(matches!(actual, Ok(0)));
            }

            #[test]
            fn test_should_parse_start_of_well_formed_range() {
                let mut headers = HeaderMap::new();
                headers.insert("content-range", "42-99".parse().unwrap());

                let actual = parse_range_start(&headers);

                assert!(matches!(actual, Ok(42)));
            }

            #[test]
            fn test_should_parse_start_when_range_has_no_end() {
                let mut headers = HeaderMap::new();
                headers.insert("content-range", "0-".parse().unwrap());

                let actual = parse_range_start(&headers);

                assert!(matches!(actual, Ok(0)));
            }

            #[test]
            fn test_should_fail_when_start_is_not_numeric() {
                let mut headers = HeaderMap::new();
                headers.insert("content-range", "oops-99".parse().unwrap());

                let actual = parse_range_start(&headers);

                assert!(matches!(actual, Err(ServerError::BadRequest(_))));
            }
        }

        mod start_upload {
            use crate::{
                UploadState,
                blob_mounter::MockBlobMounter,
                blob_uploader::MockBlobUploader,
                name::Name,
                upload::{StartParams, start_upload},
            };
            use axum::response::IntoResponse;
            use std::{str::FromStr, sync::Arc};
            use uuid::Uuid;

            #[test]
            fn test_should_return_created_when_mount_succeeds() {
                let sha = "ff".repeat(32);
                let mut blob_mounter = MockBlobMounter::new();
                let blob_uploader = MockBlobUploader::new();

                blob_mounter
                    .expect_mount_blob()
                    .returning(|_, _, _| Ok(true));

                let state = UploadState {
                    blob_uploader: Arc::new(blob_uploader),
                    blob_mounter: Arc::new(blob_mounter),
                };
                let name = Name::from_str("foo").unwrap();
                let params = StartParams {
                    mount: Some(format!("sha256:{sha}")),
                    from: None,
                };

                let actual = start_upload(state, name, params).map(IntoResponse::into_response);

                let Ok(response) = actual else {
                    panic!("expected success");
                };
                assert_eq!(response.status(), axum::http::StatusCode::CREATED);
                assert!(response.headers().contains_key("Location"));
                assert!(response.headers().contains_key("docker-content-digest"));
            }

            #[test]
            fn test_should_fall_back_to_normal_upload_when_mount_misses() {
                let sha = "ff".repeat(32);
                let mut blob_mounter = MockBlobMounter::new();
                let mut blob_uploader = MockBlobUploader::new();

                blob_mounter
                    .expect_mount_blob()
                    .returning(|_, _, _| Ok(false));
                blob_uploader.expect_start().returning(|_| Ok(Uuid::max()));

                let state = UploadState {
                    blob_uploader: Arc::new(blob_uploader),
                    blob_mounter: Arc::new(blob_mounter),
                };
                let name = Name::from_str("foo").unwrap();
                let params = StartParams {
                    mount: Some(format!("sha256:{sha}")),
                    from: None,
                };

                let actual = start_upload(state, name, params).map(IntoResponse::into_response);

                let Ok(response) = actual else {
                    panic!("expected success");
                };
                assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
            }

            #[test]
            fn test_should_skip_mounting_when_mount_param_is_absent() {
                let blob_mounter = MockBlobMounter::new();
                let mut blob_uploader = MockBlobUploader::new();

                blob_uploader.expect_start().returning(|_| Ok(Uuid::max()));

                let state = UploadState {
                    blob_uploader: Arc::new(blob_uploader),
                    blob_mounter: Arc::new(blob_mounter),
                };
                let name = Name::from_str("foo").unwrap();
                let params = StartParams {
                    mount: None,
                    from: None,
                };

                let actual = start_upload(state, name, params).map(IntoResponse::into_response);

                let Ok(response) = actual else {
                    panic!("expected success");
                };
                assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
            }

            #[test]
            fn test_should_fail_when_mount_is_not_a_valid_digest() {
                let blob_mounter = MockBlobMounter::new();
                let blob_uploader = MockBlobUploader::new();

                let state = UploadState {
                    blob_uploader: Arc::new(blob_uploader),
                    blob_mounter: Arc::new(blob_mounter),
                };
                let name = Name::from_str("foo").unwrap();
                let params = StartParams {
                    mount: Some("not-a-digest".to_string()),
                    from: None,
                };

                let actual = start_upload(state, name, params);

                assert!(actual.is_err());
            }

            #[test]
            fn test_should_ignore_malformed_from_hint() {
                let sha = "ff".repeat(32);
                let mut blob_mounter = MockBlobMounter::new();
                let blob_uploader = MockBlobUploader::new();

                blob_mounter
                    .expect_mount_blob()
                    .withf(|source_registry, _, _| source_registry.is_none())
                    .returning(|_, _, _| Ok(true));

                let state = UploadState {
                    blob_uploader: Arc::new(blob_uploader),
                    blob_mounter: Arc::new(blob_mounter),
                };
                let name = Name::from_str("foo").unwrap();
                let params = StartParams {
                    mount: Some(format!("sha256:{sha}")),
                    from: Some("Oops".to_string()),
                };

                let actual = start_upload(state, name, params).map(IntoResponse::into_response);

                assert!(actual.is_ok());
            }
        }
    }
}

mod blobs {
    use crate::{
        BlobState, ServerError,
        blob_store::{BlobStoreError, ResourceKind},
        digest::Digest,
        name::Name,
    };
    use axum::{
        body::Body,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use std::{str::FromStr, sync::Arc};
    use tokio_util::io::ReaderStream;

    fn to_blob_error(error: BlobStoreError) -> ServerError {
        match error {
            BlobStoreError::DigestNotFound(_) => ServerError::BlobUnknown(error.to_string()),
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
            .stats(&name, &digest, ResourceKind::Blob)
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
        let stats = state
            .blob_store
            .stats(&name, &digest, ResourceKind::Blob)
            .await
            .map_err(to_blob_error)?;
        let reader = state
            .blob_store
            .get(&name, &digest, ResourceKind::Blob)
            .await
            .map_err(to_blob_error)?;
        let reader = crate::stream_logging::LoggingReader::new(
            reader,
            Arc::clone(&state.reporter),
            format!("blob {name} {digest}"),
        );
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

    pub(crate) async fn delete(
        State(state): State<BlobState>,
        Path((name, raw_digest)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        delete_blob(state, Name::from_str(&name)?, raw_digest).await
    }

    pub(crate) async fn namespaced_delete(
        State(state): State<BlobState>,
        Path((namespace, name, raw_digest)): Path<(String, String, String)>,
    ) -> Result<impl IntoResponse, ServerError> {
        delete_blob(state, Name::from_namespaced(&namespace, &name)?, raw_digest).await
    }

    async fn delete_blob(
        state: BlobState,
        name: Name,
        raw_digest: String,
    ) -> Result<impl IntoResponse, ServerError> {
        let digest = Digest::from_str(&raw_digest)?;
        state
            .blob_store
            .delete(&name, &digest, ResourceKind::Blob)
            .await
            .map_err(to_blob_error)?;

        Ok(StatusCode::ACCEPTED)
    }
}
