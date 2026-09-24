use crate::{ServerError, SystemState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use resin_types::Repository;
use serde_json::{Map, Value};

pub(crate) async fn catalog(
    State(state): State<SystemState>,
) -> Result<impl IntoResponse, ServerError> {
    let names = Value::Array(
        state
            .repository_store
            .list()?
            .iter()
            .map(|repository| Value::String(repository.to_string()))
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
    State(state): State<SystemState>,
    Path(repository): Path<String>,
) -> Result<impl IntoResponse, ServerError> {
    let repository: Repository = repository.parse()?;
    let name = repository.require_local()?.clone();
    state.repository_store.delete(&Repository::Local(name))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn v2() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
    )
}
