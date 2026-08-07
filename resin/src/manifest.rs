use crate::{
    ManifestState, ServerError,
    blob_store::{BlobStore, BlobStoreError, ResourceKind},
    digest::Digest,
    tag_store::{TagStore, TagStoreError},
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use log::{Outcome, Reporter, ScopeKind, Span};
use resin_types::Name;
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
            BlobStoreError::DigestNotFound(_) => ServerError::ManifestUnknown(digest.to_string()),
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
    let guard = Span::new(Arc::clone(reporter), "Tag cleanup", ScopeKind::Task).start_guard();

    let tags = match tag_store.list(name) {
        Ok(tags) => tags,
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("failed to list tags for {name} while cleaning up after deleting {digest}: {err}"),
            );
            guard.finish_with_outcome(Outcome::Failed);
            return;
        }
    };

    let mut had_error = false;

    for tag in tags {
        let points_at_deleted_digest = tag_store
            .read(name, &tag)
            .is_ok_and(|resolved| resolved == *digest);

        if !points_at_deleted_digest {
            continue;
        }

        if let Err(err) = tag_store.delete(name, &tag) {
            had_error = true;
            guard.span().message(
                log::Level::Warn,
                &format!("failed to delete stale tag {name}:{tag} pointing at {digest}: {err}"),
            );
        }
    }

    guard.finish_with_outcome(if had_error {
        Outcome::Failed
    } else {
        Outcome::Ok
    });
}

#[cfg(test)]
mod tests {
    mod get_digest_from_reference {
        use crate::{
            ServerError,
            blob_store::{BlobStoreError, MockBlobStore},
            digest::Digest,
            manifest::get_digest_from_reference,
            tag_store::{MockTagStore, TagStoreError},
        };
        use resin_types::Name;
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
        use crate::{digest::Digest, manifest::remove_tags_pointing_to, tag_store::MockTagStore};
        use log::{Event, Reporter};
        use resin_types::Name;
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
