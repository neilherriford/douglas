use crate::UploadState;
use crate::authorize::authorize_write;
use crate::{ServerError, digest::Digest};
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use http_body::Body as HttpBody;
use resin_types::{Name, Repository};
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
    Path(repository): Path<String>,
    Query(params): Query<StartParams>,
) -> Result<impl IntoResponse, ServerError> {
    let repository: Repository = repository.parse()?;
    let name = authorize_write(&repository, state.seedling_registration_client.as_ref()).await?;
    start_upload(state, name, params)
}

fn start_upload(
    state: UploadState,
    name: Name,
    params: StartParams,
) -> Result<impl IntoResponse, ServerError> {
    let digest = params.mount.map(|raw| Digest::from_str(&raw)).transpose()?;
    let source_registry = params.from.and_then(|raw| Name::from_str(&raw).ok());

    if let Some(digest) = digest {
        let mounted = state
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
    Path((repository, uuid)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ServerError> {
    let repository: Repository = repository.parse()?;
    let name = repository.require_local()?;
    get_status(state, uuid, name.clone())
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
    Path((repository, uuid)): Path<(String, Uuid)>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ServerError> {
    let range_start = parse_range_start(&headers)?;
    let repository: Repository = repository.parse()?;
    let name = authorize_write(&repository, state.seedling_registration_client.as_ref()).await?;
    write(state, name, uuid, range_start, body).await
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
                    return Poll::Ready(Err(io::Error::other(crate::error_chain(&err))));
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
    Path((repository, uuid)): Path<(String, Uuid)>,
    Query(params): Query<CompleteParams>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ServerError> {
    let repository: Repository = repository.parse()?;
    let name = authorize_write(&repository, state.seedling_registration_client.as_ref()).await?;
    complete_upload(state, uuid, params, name, headers)
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
    Path((repository, uuid)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ServerError> {
    let repository: Repository = repository.parse()?;
    let name = authorize_write(&repository, state.seedling_registration_client.as_ref()).await?;
    abort_upload(state, name, uuid)
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
            upload::{StartParams, start_upload},
        };
        use axum::response::IntoResponse;
        use resin_types::Name;
        use seedling_registration_client::MockClient;
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
                seedling_registration_client: Arc::new(MockClient::new()),
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
                seedling_registration_client: Arc::new(MockClient::new()),
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
                seedling_registration_client: Arc::new(MockClient::new()),
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
                seedling_registration_client: Arc::new(MockClient::new()),
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
                seedling_registration_client: Arc::new(MockClient::new()),
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

    mod handlers_reject_upstream_repository {
        use crate::{
            ServerError, UploadState,
            blob_mounter::MockBlobMounter,
            blob_uploader::MockBlobUploader,
            upload::{CompleteParams, StartParams, abort, complete, start, write_chunk},
        };
        use axum::extract::{Path, Query, State};
        use axum::http::HeaderMap;
        use seedling_registration_client::MockClient;
        use std::sync::Arc;
        use uuid::Uuid;

        fn state() -> UploadState {
            let mut seedling_registration_client = MockClient::new();
            seedling_registration_client
                .expect_seedling_registered()
                .times(0);

            UploadState {
                blob_uploader: Arc::new(MockBlobUploader::new()),
                blob_mounter: Arc::new(MockBlobMounter::new()),
                seedling_registration_client: Arc::new(seedling_registration_client),
            }
        }

        #[tokio::test]
        async fn test_start_should_reject_upstream_repository() {
            let actual = start(
                State(state()),
                Path("ghcr.io/foo/bar".to_string()),
                Query(StartParams {
                    mount: None,
                    from: None,
                }),
            )
            .await;

            assert!(matches!(actual, Err(ServerError::MethodNotAllowed(_))));
        }

        #[tokio::test]
        async fn test_write_chunk_should_reject_upstream_repository() {
            let actual = write_chunk(
                State(state()),
                Path(("ghcr.io/foo/bar".to_string(), Uuid::max())),
                HeaderMap::new(),
                axum::body::Body::empty(),
            )
            .await;

            assert!(matches!(actual, Err(ServerError::MethodNotAllowed(_))));
        }

        #[tokio::test]
        async fn test_complete_should_reject_upstream_repository() {
            let actual = complete(
                State(state()),
                Path(("ghcr.io/foo/bar".to_string(), Uuid::max())),
                Query(CompleteParams {
                    digest: "sha256:ff".to_string(),
                }),
                HeaderMap::new(),
            )
            .await;

            assert!(matches!(actual, Err(ServerError::MethodNotAllowed(_))));
        }

        #[tokio::test]
        async fn test_abort_should_reject_upstream_repository() {
            let actual = abort(
                State(state()),
                Path(("ghcr.io/foo/bar".to_string(), Uuid::max())),
            )
            .await;

            assert!(matches!(actual, Err(ServerError::MethodNotAllowed(_))));
        }
    }
}
