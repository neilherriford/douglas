use crate::{RepositoryStore, ServerError};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use resin_types::Name;
use serde_json::{Map, Value};
use std::{str::FromStr, sync::Arc};

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

pub(crate) async fn delete_repository(
    State(repository_store): State<Arc<dyn RepositoryStore>>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
    let name = Name::from_str(&name)?;
    repository_store.delete(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn v2() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
    )
}
