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
