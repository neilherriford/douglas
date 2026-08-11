use crate::{
    ServerError,
    tag_store::{TagStore, TagStoreError},
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use resin_types::Name;
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
