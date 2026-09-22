use crate::{
    blob_store::{BlobStore, BlobStoreError, ResourceKind, Stats},
    digest::Digest,
    token_exchange::{DockerHubTokenExchange, TokenExchange},
};
use async_trait::async_trait;
use bytes::{Buf, Bytes};
use log::{Outcome, Reporter, ScopeKind, Span};
use resin_types::{Repository, RepositoryPath, Upstream};
use simple_rest_client::{
    Header, Request, Response, ServerClosedConnections, StreamedResponse, header_predicates,
    tls_socket::{RedirectFollowingClient, TlsRedirectFollowingClient},
};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{Mutex, mpsc, oneshot},
};

pub struct ProxyingBlobStore {
    reporter: Arc<dyn Reporter>,
    primary_blob_store: Arc<dyn BlobStore>,
    token_exchange: Box<dyn TokenExchange>,
    rest_client: Mutex<Box<dyn RedirectFollowingClient>>,
}

type BodyAsyncReader = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

impl ProxyingBlobStore {
    pub fn new(reporter: Arc<dyn Reporter>, primary_blob_store: Arc<dyn BlobStore>) -> Self {
        Self {
            reporter: Arc::clone(&reporter),
            primary_blob_store,
            token_exchange: Box::new(DockerHubTokenExchange::new(Arc::clone(&reporter))),
            rest_client: Mutex::new(Box::new(TlsRedirectFollowingClient::new(
                5,
                ServerClosedConnections::Ignore,
            ))),
        }
    }

    fn require_docker_hub(upstream: &Upstream) -> Result<(), BlobStoreError> {
        if matches!(upstream, Upstream::DockerHub) {
            Ok(())
        } else {
            Err(BlobStoreError::UnsupportedUpstream(
                upstream.canonical_host(),
            ))
        }
    }

    fn reference_path(
        path: &RepositoryPath,
        reference: &str,
        resource_kind: ResourceKind,
    ) -> String {
        let segment = match resource_kind {
            ResourceKind::Blob => "blobs",
            ResourceKind::Manifest => "manifests",
        };

        format!("/v2/{path}/{segment}/{reference}")
    }

    fn blob_path(path: &RepositoryPath, digest: &Digest, resource_kind: ResourceKind) -> String {
        Self::reference_path(path, &digest.to_string(), resource_kind)
    }

    fn failed(
        reference: impl std::fmt::Display,
        details: impl std::fmt::Display,
    ) -> BlobStoreError {
        BlobStoreError::FailedToRetrieveDigest {
            digest: reference.to_string(),
            details: details.to_string(),
        }
    }

    const REMOTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

    async fn with_timeout<T>(
        &self,
        action_description: &str,
        reference: impl std::fmt::Display,
        future: impl Future<Output = Result<T, BlobStoreError>>,
    ) -> Result<T, BlobStoreError> {
        self.with_timeout_after(Self::REMOTE_TIMEOUT, action_description, reference, future)
            .await
    }

    async fn with_timeout_after<T>(
        &self,
        duration: std::time::Duration,
        action_description: &str,
        reference: impl std::fmt::Display,
        future: impl Future<Output = Result<T, BlobStoreError>>,
    ) -> Result<T, BlobStoreError> {
        match tokio::time::timeout(duration, future).await {
            Ok(result) => result,
            Err(_) => {
                let guard = Span::new(
                    Arc::clone(&self.reporter),
                    "Remote timeout",
                    ScopeKind::Task,
                )
                .start_guard();
                let message =
                    format!("{action_description} for {reference} timed out after {duration:?}");
                guard.span().message(log::Level::Warn, &message);
                guard.finish_with_outcome(Outcome::Failed);
                Err(Self::failed(reference, message))
            }
        }
    }

    async fn remote_token(
        &self,
        path: &RepositoryPath,
        digest: &Digest,
    ) -> Result<String, BlobStoreError> {
        self.with_timeout("token fetch", digest, async {
            self.token_exchange
                .fetch_token(path)
                .await
                .map_err(|err| Self::failed(digest, err))
        })
        .await
    }

    async fn remote_get(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<(String, BodyAsyncReader), BlobStoreError> {
        Self::require_docker_hub(upstream)?;
        let token = self.remote_token(path, digest).await?;

        let request = Request::Get {
            path: Self::blob_path(path, digest, resource_kind),
            headers: vec![Header::authorization_bearer(&token)],
            query: HashMap::new(),
        };

        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Remote blob get",
            ScopeKind::Task,
        )
        .start_guard();
        let span = guard.span().clone();
        let api_host = upstream.api_host();

        let result = self
            .with_timeout("streaming GET", digest, async move {
                self.rest_client
                    .lock()
                    .await
                    .execute_streaming(&span, &api_host, request)
                    .await
                    .map_err(|err| Self::failed(digest, err))
            })
            .await;

        guard.finish_with_outcome(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Failed
        });
        let response = result?;

        match response {
            StreamedResponse::Okay { body, headers }
            | StreamedResponse::Created { body, headers } => {
                let media_type = Self::get_media_type(&headers, digest)?;

                Ok((media_type, body))
            }
            StreamedResponse::NoContent { .. } => {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            }
            // See the matching arm in remote_stats: Docker Hub's anonymous
            // 401 means the same thing a 404 would here.
            StreamedResponse::Error { status: 404, .. } => {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            }
            StreamedResponse::Error { status: 401, .. }
                if matches!(upstream, Upstream::DockerHub) =>
            {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            }
            StreamedResponse::Error { status, body, .. } => Err(Self::failed(
                digest,
                format!("status {status}: {}", body.unwrap_or_default()),
            )),
        }
    }

    async fn remote_exists(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<bool, BlobStoreError> {
        match self
            .remote_stats(upstream, path, digest, resource_kind)
            .await
        {
            Ok(_) => Ok(true),
            Err(BlobStoreError::DigestNotFound(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn remote_stats(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<Stats, BlobStoreError> {
        Self::require_docker_hub(upstream)?;
        let token = self.remote_token(path, digest).await?;

        let request = Request::Head {
            path: Self::blob_path(path, digest, resource_kind),
            headers: vec![Header::authorization_bearer(&token)],
            query: HashMap::new(),
        };

        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Remote blob exist",
            ScopeKind::Task,
        )
        .start_guard();
        let span = guard.span().clone();
        let api_host = upstream.api_host();

        let result = self
            .with_timeout("HEAD", digest, async move {
                self.rest_client
                    .lock()
                    .await
                    .execute(&span, &api_host, request)
                    .await
                    .map_err(|err| Self::failed(digest, err))
            })
            .await;

        guard.finish_with_outcome(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Failed
        });
        let response = result?;

        match response {
            Response::Okay { headers, .. } => {
                let mediatype = Self::get_media_type(&headers, digest)?;
                let size = Self::get_header_value(
                    &headers,
                    header_predicates::is_content_length(),
                    || Self::failed(digest, "Missing content length"),
                )?
                .parse::<u64>()
                .map_err(|err| Self::failed(digest, format!("Invalid content length: {err}")))?;

                Ok(Stats { mediatype, size })
            }
            Response::Created { .. } | Response::NoContent { .. } => {
                Err(Self::failed(digest, "Unexpected response"))
            }
            // Docker Hub returns 401 rather than 404 for a repo it doesn't
            // recognize, even for anonymous pulls — it doesn't distinguish
            // "doesn't exist" from "exists but you can't see it", so both
            // collapse to 401/insufficient_scope. Since we only ever call
            // Docker Hub anonymously here, a 401 means "no such public repo",
            // same as a 404. Other registries don't share that quirk.
            Response::Error { status: 404, .. } => {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            }
            Response::Error { status: 401, .. } if matches!(upstream, Upstream::DockerHub) => {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            }
            Response::Error { status, body, .. } => Err(Self::failed(
                digest,
                format!("status {status}: {}", body.unwrap_or_default()),
            )),
        }
    }

    async fn remote_resolve_reference(
        &self,
        upstream: &Upstream,
        path: &RepositoryPath,
        reference: &str,
        resource_kind: ResourceKind,
    ) -> Result<Digest, BlobStoreError> {
        Self::require_docker_hub(upstream)?;
        let token = self
            .token_exchange
            .fetch_token(path)
            .await
            .map_err(|err| Self::failed(reference, err))?;

        let request = Request::Head {
            path: Self::reference_path(path, reference, resource_kind),
            headers: vec![Header::authorization_bearer(&token)],
            query: HashMap::new(),
        };

        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Remote reference resolve",
            ScopeKind::Task,
        )
        .start_guard();
        let span = guard.span().clone();
        let api_host = upstream.api_host();

        let result = self
            .with_timeout("HEAD", reference, async move {
                self.rest_client
                    .lock()
                    .await
                    .execute(&span, &api_host, request)
                    .await
                    .map_err(|err| Self::failed(reference, err))
            })
            .await;

        guard.finish_with_outcome(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Failed
        });
        let response = result?;

        match response {
            Response::Okay { headers, .. } => {
                let digest_header = Self::get_header_value(
                    &headers,
                    header_predicates::named("docker-content-digest"),
                    || Self::failed(reference, "Missing Docker-Content-Digest header"),
                )?;

                digest_header
                    .parse::<Digest>()
                    .map_err(|err| Self::failed(reference, err))
            }
            Response::Created { .. } | Response::NoContent { .. } => {
                Err(Self::failed(reference, "Unexpected response"))
            }
            // See the matching arm in remote_stats: Docker Hub's anonymous
            // 401 means the same thing a 404 would here.
            Response::Error { status: 404, .. } => {
                Err(BlobStoreError::DigestNotFound(reference.to_string()))
            }
            Response::Error { status: 401, .. } if matches!(upstream, Upstream::DockerHub) => {
                Err(BlobStoreError::DigestNotFound(reference.to_string()))
            }
            Response::Error { status, body, .. } => Err(Self::failed(
                reference,
                format!("status {status}: {}", body.unwrap_or_default()),
            )),
        }
    }

    fn log_local_error(
        &self,
        operation: &str,
        repository: &Repository,
        reference: impl std::fmt::Display,
        err: &BlobStoreError,
    ) {
        let guard =
            Span::new(Arc::clone(&self.reporter), "Cache lookup", ScopeKind::Task).start_guard();
        guard.span().message(
            log::Level::Warn,
            &format!("{operation} {repository} {reference}: local lookup failed, not falling back to remote: {err}"),
        );
        guard.finish_with_outcome(Outcome::Failed);
    }

    fn log_cache_decision(
        &self,
        operation: &str,
        repository: &Repository,
        reference: impl std::fmt::Display,
        hit: bool,
    ) {
        let guard =
            Span::new(Arc::clone(&self.reporter), "Cache lookup", ScopeKind::Task).start_guard();
        let outcome = if hit {
            "hit"
        } else {
            "miss, falling back to remote"
        };
        guard.span().message(
            log::Level::Info,
            &format!("{operation} {repository} {reference}: local cache {outcome}"),
        );
        guard.finish_with_outcome(Outcome::Ok);
    }

    fn get_header_value(
        headers: &[Header],
        predicate: impl Fn(&&Header) -> bool,
        create_error: impl Fn() -> BlobStoreError,
    ) -> Result<String, BlobStoreError> {
        headers
            .iter()
            .find(predicate)
            .map(|header| header.value.clone())
            .ok_or_else(create_error)
    }

    fn get_media_type(headers: &[Header], digest: &Digest) -> Result<String, BlobStoreError> {
        Self::get_header_value(headers, header_predicates::is_content_type(), || {
            Self::failed(digest, "Response did not include media type")
        })
    }
}

struct TeeReader {
    inner: BodyAsyncReader,
    sink: mpsc::UnboundedSender<Bytes>,
}

impl TeeReader {
    fn new(inner: BodyAsyncReader, sink: mpsc::UnboundedSender<Bytes>) -> Self {
        Self { inner, sink }
    }
}

impl AsyncRead for TeeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();

        let result = Pin::new(&mut this.inner).poll_read(cx, buf);

        if let Poll::Ready(Ok(())) = &result {
            let filled = &buf.filled()[before..];
            if !filled.is_empty() {
                let _ = this.sink.send(Bytes::copy_from_slice(filled));
            }
        }

        result
    }
}

struct VerifiedCacheReader {
    receiver: mpsc::UnboundedReceiver<Bytes>,
    outcome: Option<oneshot::Receiver<Result<(), BlobStoreError>>>,
    buffer: Bytes,
}

impl VerifiedCacheReader {
    fn new(
        receiver: mpsc::UnboundedReceiver<Bytes>,
        outcome: oneshot::Receiver<Result<(), BlobStoreError>>,
    ) -> Self {
        Self {
            receiver,
            outcome: Some(outcome),
            buffer: Bytes::new(),
        }
    }
}

impl AsyncRead for VerifiedCacheReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        loop {
            if !this.buffer.is_empty() {
                let amount = std::cmp::min(buf.remaining(), this.buffer.len());
                buf.put_slice(&this.buffer[..amount]);
                this.buffer.advance(amount);
                return Poll::Ready(Ok(()));
            }

            match this.receiver.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => {
                    this.buffer = chunk;
                    continue;
                }
                Poll::Ready(None) => {
                    let Some(outcome) = this.outcome.as_mut() else {
                        return Poll::Ready(Ok(()));
                    };

                    return match Pin::new(outcome).poll(cx) {
                        Poll::Ready(Ok(Ok(()))) => {
                            this.outcome = None;
                            Poll::Ready(Ok(()))
                        }
                        Poll::Ready(Ok(Err(err))) => {
                            this.outcome = None;
                            Poll::Ready(Err(std::io::Error::other(err)))
                        }
                        Poll::Ready(Err(_)) => {
                            this.outcome = None;
                            Poll::Ready(Err(std::io::Error::other(
                                "cache write task ended without a result",
                            )))
                        }
                        Poll::Pending => Poll::Pending,
                    };
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[async_trait]
impl BlobStore for ProxyingBlobStore {
    async fn save(
        &self,
        repository: &Repository,
        claimed: &Digest,
        reader: BodyAsyncReader,
        mediatype: &str,
        resource_kind: ResourceKind,
    ) -> Result<(), BlobStoreError> {
        repository.require_local()?;
        self.primary_blob_store
            .save(repository, claimed, reader, mediatype, resource_kind)
            .await
    }

    async fn get(
        &self,
        repository: &Repository,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<BodyAsyncReader, BlobStoreError> {
        match self
            .primary_blob_store
            .get(repository, digest, resource_kind)
            .await
        {
            Ok(reader) => {
                self.log_cache_decision("get", repository, digest, true);
                Ok(reader)
            }
            Err(BlobStoreError::DigestNotFound(_)) => {
                let Some((upstream, path)) = repository.as_upstream() else {
                    self.log_cache_decision("get", repository, digest, false);
                    return Err(BlobStoreError::DigestNotFound(digest.to_string()));
                };
                self.log_cache_decision("get", repository, digest, false);
                let (media_type, remote_body) = self
                    .remote_get(upstream, path, digest, resource_kind)
                    .await?;

                let (sink, receiver) = mpsc::unbounded_channel();
                let (done_tx, done_rx) = oneshot::channel();
                let tee = TeeReader::new(remote_body, sink);

                let primary_blob_store = Arc::clone(&self.primary_blob_store);
                let reporter = Arc::clone(&self.reporter);
                let repository = repository.clone();
                let digest = digest.clone();

                tokio::spawn(async move {
                    let result = primary_blob_store
                        .save(
                            &repository,
                            &digest,
                            Box::new(tee),
                            &media_type,
                            resource_kind,
                        )
                        .await;

                    let guard = Span::new(Arc::clone(&reporter), "Cache write", ScopeKind::Task)
                        .start_guard();
                    match &result {
                        Ok(()) => {
                            guard.span().message(
                                log::Level::Info,
                                &format!("cached {repository} {digest} ({resource_kind:?})"),
                            );
                            guard.finish_with_outcome(Outcome::Ok);
                        }
                        Err(err) => {
                            guard.span().message(
                                log::Level::Warn,
                                &format!(
                                    "failed to cache {repository} {digest} ({resource_kind:?}): {err}"
                                ),
                            );
                            guard.finish_with_outcome(Outcome::Failed);
                        }
                    }

                    let _ = done_tx.send(result);
                });

                Ok(Box::new(VerifiedCacheReader::new(receiver, done_rx)) as BodyAsyncReader)
            }
            Err(err) => {
                self.log_local_error("get", repository, digest, &err);
                Err(err)
            }
        }
    }

    async fn exists(
        &self,
        repository: &Repository,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<bool, BlobStoreError> {
        match self
            .primary_blob_store
            .exists(repository, digest, resource_kind)
            .await
        {
            Ok(true) => {
                self.log_cache_decision("exists", repository, digest, true);
                Ok(true)
            }
            Ok(false) => {
                self.log_cache_decision("exists", repository, digest, false);
                let Some((upstream, path)) = repository.as_upstream() else {
                    return Ok(false);
                };
                self.remote_exists(upstream, path, digest, resource_kind)
                    .await
            }
            Err(err) => {
                self.log_local_error("exists", repository, digest, &err);
                Err(err)
            }
        }
    }

    async fn stats(
        &self,
        repository: &Repository,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<Stats, BlobStoreError> {
        match self
            .primary_blob_store
            .stats(repository, digest, resource_kind)
            .await
        {
            Ok(stats) => {
                self.log_cache_decision("stats", repository, digest, true);
                Ok(stats)
            }
            Err(BlobStoreError::DigestNotFound(_)) => {
                let Some((upstream, path)) = repository.as_upstream() else {
                    self.log_cache_decision("stats", repository, digest, false);
                    return Err(BlobStoreError::DigestNotFound(digest.to_string()));
                };
                self.log_cache_decision("stats", repository, digest, false);
                self.remote_stats(upstream, path, digest, resource_kind)
                    .await
            }
            Err(err) => {
                self.log_local_error("stats", repository, digest, &err);
                Err(err)
            }
        }
    }

    async fn delete(
        &self,
        repository: &Repository,
        digest: &Digest,
        resource_kind: ResourceKind,
    ) -> Result<(), BlobStoreError> {
        repository.require_local()?;
        self.primary_blob_store
            .delete(repository, digest, resource_kind)
            .await
    }

    async fn resolve_reference(
        &self,
        repository: &Repository,
        reference: &str,
        resource_kind: ResourceKind,
    ) -> Result<Digest, BlobStoreError> {
        match self
            .primary_blob_store
            .resolve_reference(repository, reference, resource_kind)
            .await
        {
            Ok(digest) => {
                self.log_cache_decision("resolve_reference", repository, reference, true);
                Ok(digest)
            }
            Err(BlobStoreError::DigestNotFound(_)) => {
                let Some((upstream, path)) = repository.as_upstream() else {
                    self.log_cache_decision("resolve_reference", repository, reference, false);
                    return Err(BlobStoreError::DigestNotFound(reference.to_string()));
                };
                self.log_cache_decision("resolve_reference", repository, reference, false);
                self.remote_resolve_reference(upstream, path, reference, resource_kind)
                    .await
            }
            Err(err) => {
                self.log_local_error("resolve_reference", repository, reference, &err);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::MockBlobStore;
    use crate::token_exchange::MockTokenExchange;
    use log::Event;
    use simple_rest_client::tls_socket::MockRedirectFollowingClient;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn test_reporter() -> Arc<dyn Reporter> {
        Arc::new(NullReporter)
    }

    fn test_repository() -> Repository {
        "foo".parse().unwrap()
    }

    fn test_upstream_repository() -> Repository {
        "docker.io/foo".parse().unwrap()
    }

    fn test_digest() -> Digest {
        Digest::from_hex(&"a".repeat(64)).unwrap()
    }

    async fn reader_of(bytes: &'static [u8]) -> BodyAsyncReader {
        let (mut writer, reader) = tokio::io::duplex(bytes.len().max(1));
        writer.write_all(bytes).await.unwrap();
        drop(writer);
        Box::new(reader)
    }

    fn build_store(
        primary_blob_store: Arc<dyn BlobStore>,
        token_exchange: MockTokenExchange,
        rest_client: MockRedirectFollowingClient,
    ) -> ProxyingBlobStore {
        ProxyingBlobStore {
            reporter: test_reporter(),
            primary_blob_store,
            token_exchange: Box::new(token_exchange),
            rest_client: Mutex::new(Box::new(rest_client)),
        }
    }

    mod blob_path {
        use super::*;

        fn test_path() -> RepositoryPath {
            let repository: Repository = "docker.io/foo".parse().unwrap();
            let Some((_, path)) = repository.as_upstream() else {
                panic!("expected an upstream repository");
            };
            path.clone()
        }

        #[test]
        fn test_blob_path_should_use_the_repository_path_as_is() {
            let path = test_path();
            let digest = test_digest();

            assert_eq!(
                ProxyingBlobStore::blob_path(&path, &digest, ResourceKind::Blob),
                format!("/v2/{path}/blobs/{digest}")
            );
        }

        #[test]
        fn test_blob_path_should_use_the_manifests_segment_for_manifests() {
            let path = test_path();
            let digest = test_digest();

            assert_eq!(
                ProxyingBlobStore::blob_path(&path, &digest, ResourceKind::Manifest),
                format!("/v2/{path}/manifests/{digest}")
            );
        }
    }

    mod failed {
        use super::*;

        #[test]
        fn test_failed_should_build_a_failed_to_retrieve_digest_error() {
            let digest = test_digest();

            match ProxyingBlobStore::failed(&digest, "whoops") {
                BlobStoreError::FailedToRetrieveDigest {
                    digest: reported_digest,
                    details,
                } => {
                    assert_eq!(reported_digest, digest.to_string());
                    assert_eq!(details, "whoops");
                }
                other => panic!("expected FailedToRetrieveDigest, got {other:?}"),
            }
        }
    }

    mod get_header_value {
        use super::*;

        #[test]
        fn test_get_header_value_should_find_a_matching_header() {
            let headers = vec![Header::new("content-type", "application/json")];

            let result = ProxyingBlobStore::get_header_value(
                &headers,
                header_predicates::is_content_type(),
                || panic!("error factory should not be called"),
            );

            assert_eq!(result.unwrap(), "application/json");
        }

        #[test]
        fn test_get_header_value_should_call_the_error_factory_when_missing() {
            let headers: Vec<Header> = vec![];
            let digest = test_digest();

            let result = ProxyingBlobStore::get_header_value(
                &headers,
                header_predicates::is_content_type(),
                || ProxyingBlobStore::failed(&digest, "missing"),
            );

            assert!(result.is_err());
        }
    }

    mod get_media_type {
        use super::*;

        #[test]
        fn test_get_media_type_should_extract_the_content_type_header() {
            let headers = vec![Header::new(
                "Content-Type",
                "application/vnd.docker.image.rootfs.diff.tar.gzip",
            )];
            let digest = test_digest();

            let result = ProxyingBlobStore::get_media_type(&headers, &digest);

            assert_eq!(
                result.unwrap(),
                "application/vnd.docker.image.rootfs.diff.tar.gzip"
            );
        }

        #[test]
        fn test_get_media_type_should_error_when_missing() {
            let digest = test_digest();

            let result = ProxyingBlobStore::get_media_type(&Vec::new(), &digest);

            assert!(result.is_err());
        }
    }

    mod tee_reader {
        use super::*;

        #[tokio::test]
        async fn test_tee_reader_should_mirror_bytes_while_passing_them_through() {
            let source = reader_of(b"Lorem ipsum dolor sit amet").await;
            let (sink, mut receiver) = mpsc::unbounded_channel();
            let mut tee = TeeReader::new(source, sink);

            let mut buf = Vec::new();
            tee.read_to_end(&mut buf).await.unwrap();
            drop(tee);

            assert_eq!(buf, b"Lorem ipsum dolor sit amet");

            let mut mirrored = Vec::new();
            while let Some(chunk) = receiver.recv().await {
                mirrored.extend_from_slice(&chunk);
            }
            assert_eq!(mirrored, b"Lorem ipsum dolor sit amet");
        }

        #[tokio::test]
        async fn test_tee_reader_should_not_fail_when_the_receiver_is_already_dropped() {
            let source = reader_of(b"Lorem ipsum dolor sit amet").await;
            let (sink, receiver) = mpsc::unbounded_channel();
            drop(receiver);
            let mut tee = TeeReader::new(source, sink);

            let mut buf = Vec::new();
            let result = tee.read_to_end(&mut buf).await;

            assert!(result.is_ok());
            assert_eq!(buf, b"Lorem ipsum dolor sit amet");
        }
    }

    mod verified_cache_reader {
        use super::*;

        #[tokio::test]
        async fn test_verified_cache_reader_should_report_clean_eof_when_save_succeeds() {
            let (sink, receiver) = mpsc::unbounded_channel();
            let (done_tx, done_rx) = oneshot::channel();
            sink.send(Bytes::from_static(b"Lorem ipsum dolor sit amet"))
                .unwrap();
            drop(sink);
            done_tx.send(Ok(())).unwrap();

            let mut reader = VerifiedCacheReader::new(receiver, done_rx);
            let mut buf = Vec::new();
            let result = reader.read_to_end(&mut buf).await;

            assert!(result.is_ok());
            assert_eq!(buf, b"Lorem ipsum dolor sit amet");
        }

        #[tokio::test]
        async fn test_verified_cache_reader_should_surface_an_error_when_save_fails() {
            let (sink, receiver) = mpsc::unbounded_channel();
            let (done_tx, done_rx) = oneshot::channel();
            sink.send(Bytes::from_static(b"Lorem ipsum dolor sit amet"))
                .unwrap();
            drop(sink);
            done_tx
                .send(Err(BlobStoreError::DigestNotFound("oops".to_string())))
                .unwrap();

            let mut reader = VerifiedCacheReader::new(receiver, done_rx);
            let mut buf = Vec::new();
            let result = reader.read_to_end(&mut buf).await;

            assert!(result.is_err());
            assert_eq!(buf, b"Lorem ipsum dolor sit amet");
        }

        #[tokio::test]
        async fn test_verified_cache_reader_should_error_when_the_save_task_ends_without_a_result()
        {
            let (sink, receiver) = mpsc::unbounded_channel::<Bytes>();
            let (done_tx, done_rx) = oneshot::channel::<Result<(), BlobStoreError>>();
            drop(sink);
            drop(done_tx);

            let mut reader = VerifiedCacheReader::new(receiver, done_rx);
            let mut buf = Vec::new();
            let result = reader.read_to_end(&mut buf).await;

            assert!(result.is_err());
        }
    }

    mod get {
        use super::*;

        #[tokio::test]
        async fn test_get_should_return_the_local_reader_on_a_cache_hit() {
            let cached = reader_of(b"cached").await;
            let mut primary = MockBlobStore::new();
            primary.expect_get().return_once(move |_, _, _| Ok(cached));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let mut reader = store
                .get(&repository, &digest, ResourceKind::Blob)
                .await
                .unwrap();

            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"cached");
        }

        #[tokio::test]
        async fn test_get_should_fetch_and_cache_remotely_on_a_cache_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_get().return_once(|_, digest, _| {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            });
            primary
                .expect_save()
                .once()
                .returning(|_, _, mut reader, _, _| {
                    // Drain the tee'd reader in the background
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let _ = reader.read_to_end(&mut buf).await;
                    });
                    Ok(())
                });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let body = reader_of(b"Lorem ipsum dolor sit amet").await;
            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client
                .expect_execute_streaming()
                .return_once(move |_, _, _| {
                    Ok(StreamedResponse::Okay {
                        headers: vec![Header::new("content-type", "application/octet-stream")],
                        body,
                    })
                });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_upstream_repository();
            let digest = test_digest();
            let mut reader = store
                .get(&repository, &digest, ResourceKind::Blob)
                .await
                .unwrap();

            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"Lorem ipsum dolor sit amet");
        }

        #[tokio::test]
        async fn test_get_should_not_fall_back_to_remote_for_a_non_digest_not_found_local_error() {
            let mut primary = MockBlobStore::new();
            primary.expect_get().return_once(|_, digest, _| {
                Err(BlobStoreError::FailedToRetrieveDigest {
                    digest: digest.to_string(),
                    details: "permission denied".to_string(),
                })
            });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let result = store.get(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details == "permission denied"
            ));
        }

        #[tokio::test]
        async fn test_get_should_not_fall_back_to_remote_for_a_local_repository_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_get().return_once(|_, digest, _| {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let result = store.get(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(result, Err(BlobStoreError::DigestNotFound(_))));
        }
    }

    mod exists {
        use super::*;

        #[tokio::test]
        async fn test_exists_should_short_circuit_on_a_local_hit() {
            let mut primary = MockBlobStore::new();
            primary.expect_exists().return_once(|_, _, _| Ok(true));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            assert!(
                store
                    .exists(&repository, &digest, ResourceKind::Blob)
                    .await
                    .unwrap()
            );
        }

        #[tokio::test]
        async fn test_exists_should_check_remotely_on_a_local_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_exists().return_once(|_, _, _| Ok(false));

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Okay {
                    headers: vec![
                        Header::new("content-type", "application/octet-stream"),
                        Header::new("content-length", "42"),
                    ],
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_upstream_repository();
            let digest = test_digest();
            assert!(
                store
                    .exists(&repository, &digest, ResourceKind::Blob)
                    .await
                    .unwrap()
            );
        }

        #[tokio::test]
        async fn test_exists_should_return_false_when_remote_reports_404() {
            let mut primary = MockBlobStore::new();
            primary.expect_exists().return_once(|_, _, _| Ok(false));

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Error {
                    headers: vec![],
                    status: 404,
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_repository();
            let digest = test_digest();
            assert!(
                !store
                    .exists(&repository, &digest, ResourceKind::Blob)
                    .await
                    .unwrap()
            );
        }

        #[tokio::test]
        async fn test_exists_should_not_fall_back_to_remote_for_a_non_digest_not_found_local_error()
        {
            let mut primary = MockBlobStore::new();
            primary.expect_exists().return_once(|_, digest, _| {
                Err(BlobStoreError::FailedToRetrieveDigest {
                    digest: digest.to_string(),
                    details: "permission denied".to_string(),
                })
            });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let result = store.exists(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details == "permission denied"
            ));
        }

        #[tokio::test]
        async fn test_exists_should_not_fall_back_to_remote_for_a_local_repository_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_exists().return_once(|_, _, _| Ok(false));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            assert!(
                !store
                    .exists(&repository, &digest, ResourceKind::Blob)
                    .await
                    .unwrap()
            );
        }
    }

    mod stats {
        use super::*;

        #[tokio::test]
        async fn test_stats_should_short_circuit_on_a_local_hit() {
            let expected = Stats {
                size: 123,
                mediatype: "application/octet-stream".to_string(),
            };
            let mut primary = MockBlobStore::new();
            let returned = expected.clone();
            primary
                .expect_stats()
                .return_once(move |_, _, _| Ok(returned));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            assert_eq!(
                store
                    .stats(&repository, &digest, ResourceKind::Blob)
                    .await
                    .unwrap(),
                expected
            );
        }

        #[tokio::test]
        async fn test_stats_should_fetch_remotely_on_a_local_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_stats().return_once(|_, digest, _| {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Okay {
                    headers: vec![
                        Header::new("content-type", "application/octet-stream"),
                        Header::new("content-length", "42"),
                    ],
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_upstream_repository();
            let digest = test_digest();
            let stats = store
                .stats(&repository, &digest, ResourceKind::Blob)
                .await
                .unwrap();

            assert_eq!(stats.size, 42);
            assert_eq!(stats.mediatype, "application/octet-stream");
        }

        #[tokio::test]
        async fn test_stats_should_return_digest_not_found_when_remote_returns_401() {
            let mut primary = MockBlobStore::new();
            primary.expect_stats().return_once(|_, digest, _| {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Error {
                    headers: vec![],
                    status: 401,
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_repository();
            let digest = test_digest();
            let result = store.stats(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(result, Err(BlobStoreError::DigestNotFound(_))));
        }

        #[tokio::test]
        async fn test_stats_should_not_fall_back_to_remote_for_a_non_digest_not_found_local_error()
        {
            let mut primary = MockBlobStore::new();
            primary.expect_stats().return_once(|_, digest, _| {
                Err(BlobStoreError::FailedToRetrieveDigest {
                    digest: digest.to_string(),
                    details: "permission denied".to_string(),
                })
            });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let result = store.stats(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details == "permission denied"
            ));
        }

        #[tokio::test]
        async fn test_stats_should_not_fall_back_to_remote_for_a_local_repository_miss() {
            let mut primary = MockBlobStore::new();
            primary.expect_stats().return_once(|_, digest, _| {
                Err(BlobStoreError::DigestNotFound(digest.to_string()))
            });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let result = store.stats(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(result, Err(BlobStoreError::DigestNotFound(_))));
        }
    }

    mod resolve_reference {
        use super::*;

        #[tokio::test]
        async fn test_resolve_reference_should_short_circuit_on_a_local_hit() {
            let digest = test_digest();
            let returned = digest.clone();
            let mut primary = MockBlobStore::new();
            primary
                .expect_resolve_reference()
                .return_once(move |_, _, _| Ok(returned));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let result = store
                .resolve_reference(&repository, "latest", ResourceKind::Manifest)
                .await;

            assert_eq!(result.unwrap(), digest);
        }

        #[tokio::test]
        async fn test_resolve_reference_should_resolve_remotely_on_a_local_miss() {
            let mut primary = MockBlobStore::new();
            primary
                .expect_resolve_reference()
                .return_once(|_, reference, _| {
                    Err(BlobStoreError::DigestNotFound(reference.to_string()))
                });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let digest = test_digest();
            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(move |_, _, _| {
                Ok(Response::Okay {
                    headers: vec![Header::new("docker-content-digest", &digest.to_string())],
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_upstream_repository();
            let result = store
                .resolve_reference(&repository, "latest", ResourceKind::Manifest)
                .await;

            assert_eq!(result.unwrap(), test_digest());
        }

        #[tokio::test]
        async fn test_resolve_reference_should_return_digest_not_found_when_remote_returns_404() {
            let mut primary = MockBlobStore::new();
            primary
                .expect_resolve_reference()
                .return_once(|_, reference, _| {
                    Err(BlobStoreError::DigestNotFound(reference.to_string()))
                });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Error {
                    headers: vec![],
                    status: 404,
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_repository();
            let result = store
                .resolve_reference(&repository, "latest", ResourceKind::Manifest)
                .await;

            assert!(
                matches!(result, Err(BlobStoreError::DigestNotFound(reference)) if reference == "latest")
            );
        }

        #[tokio::test]
        async fn test_resolve_reference_should_fail_when_remote_response_is_missing_the_digest_header()
         {
            let mut primary = MockBlobStore::new();
            primary
                .expect_resolve_reference()
                .return_once(|_, reference, _| {
                    Err(BlobStoreError::DigestNotFound(reference.to_string()))
                });

            let mut token_exchange = MockTokenExchange::new();
            token_exchange
                .expect_fetch_token()
                .return_once(|_| Ok("token".to_string()));

            let mut rest_client = MockRedirectFollowingClient::new();
            rest_client.expect_execute().return_once(|_, _, _| {
                Ok(Response::Okay {
                    headers: vec![],
                    body: None,
                })
            });

            let store = build_store(Arc::new(primary), token_exchange, rest_client);

            let repository = test_upstream_repository();
            let result = store
                .resolve_reference(&repository, "latest", ResourceKind::Manifest)
                .await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details.contains("Docker-Content-Digest")
            ));
        }

        #[tokio::test]
        async fn test_resolve_reference_should_not_fall_back_to_remote_for_a_non_digest_not_found_local_error()
         {
            let mut primary = MockBlobStore::new();
            primary
                .expect_resolve_reference()
                .return_once(|_, reference, _| {
                    Err(BlobStoreError::FailedToRetrieveDigest {
                        digest: reference.to_string(),
                        details: "permission denied".to_string(),
                    })
                });

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let result = store
                .resolve_reference(&repository, "latest", ResourceKind::Manifest)
                .await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details == "permission denied"
            ));
        }
    }

    mod with_timeout {
        use super::*;
        use std::time::Duration;

        #[tokio::test]
        async fn test_with_timeout_after_should_return_the_future_result_when_it_completes_in_time()
        {
            let store = build_store(
                Arc::new(MockBlobStore::new()),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let result = store
                .with_timeout_after(Duration::from_secs(5), "test op", "ref", async {
                    Ok::<_, BlobStoreError>(42)
                })
                .await;

            assert_eq!(result.unwrap(), 42);
        }

        #[tokio::test]
        async fn test_with_timeout_after_should_fail_when_the_future_is_too_slow() {
            let store = build_store(
                Arc::new(MockBlobStore::new()),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let result = store
                .with_timeout_after(Duration::from_millis(10), "test op", "ref", async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok::<_, BlobStoreError>(42)
                })
                .await;

            assert!(matches!(
                result,
                Err(BlobStoreError::FailedToRetrieveDigest { details, .. })
                    if details.contains("test op") && details.contains("timed out")
            ));
        }
    }

    mod save {
        use super::*;

        #[tokio::test]
        async fn test_save_should_delegate_to_the_primary_blob_store() {
            let mut primary = MockBlobStore::new();
            primary
                .expect_save()
                .once()
                .returning(|_, _, _, _, _| Ok(()));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();
            let reader = reader_of(b"blob").await;

            store
                .save(
                    &repository,
                    &digest,
                    reader,
                    "application/octet-stream",
                    ResourceKind::Blob,
                )
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn test_save_should_reject_an_upstream_repository() {
            let store = build_store(
                Arc::new(MockBlobStore::new()),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_upstream_repository();
            let digest = test_digest();
            let reader = reader_of(b"blob").await;

            let result = store
                .save(
                    &repository,
                    &digest,
                    reader,
                    "application/octet-stream",
                    ResourceKind::Blob,
                )
                .await;

            assert!(matches!(result, Err(BlobStoreError::NotLocal(_))));
        }
    }

    mod delete {
        use super::*;

        #[tokio::test]
        async fn test_delete_should_delegate_to_the_primary_blob_store() {
            let mut primary = MockBlobStore::new();
            primary.expect_delete().return_once(|_, _, _| Ok(()));

            let store = build_store(
                Arc::new(primary),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_repository();
            let digest = test_digest();

            store
                .delete(&repository, &digest, ResourceKind::Blob)
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn test_delete_should_reject_an_upstream_repository() {
            let store = build_store(
                Arc::new(MockBlobStore::new()),
                MockTokenExchange::new(),
                MockRedirectFollowingClient::new(),
            );

            let repository = test_upstream_repository();
            let digest = test_digest();

            let result = store.delete(&repository, &digest, ResourceKind::Blob).await;

            assert!(matches!(result, Err(BlobStoreError::NotLocal(_))));
        }
    }

    mod require_docker_hub {
        use super::*;
        use resin_types::Upstream;

        #[test]
        fn test_should_accept_docker_hub() {
            assert!(ProxyingBlobStore::require_docker_hub(&Upstream::DockerHub).is_ok());
        }

        #[test]
        fn test_should_reject_another_registry() {
            let upstream = Upstream::Other("ghcr.io".parse().unwrap());

            let result = ProxyingBlobStore::require_docker_hub(&upstream);

            assert!(
                matches!(result, Err(BlobStoreError::UnsupportedUpstream(host)) if host == "ghcr.io")
            );
        }
    }
}
