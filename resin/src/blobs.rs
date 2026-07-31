use crate::{
    BlobState, ServerError,
    blob_store::{BlobStoreError, ResourceKind},
    digest::Digest,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use resin_types::Name;
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
