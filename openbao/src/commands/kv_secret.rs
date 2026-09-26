use crate::{
    Error,
    commands::{DataWrapper, KeysData, configure_kv::ensure_key_value, open_bao_token_header},
};
use log::{Reporter, Span};
use openbao_types::{KvPath, Mounts, SecretRecord, SecretValue};
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, Response, RestClient,
    assertions::{assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Serialize)]
struct CasOptions {
    cas: u32,
}

#[derive(Debug, Serialize)]
struct WriteRequest<'a> {
    options: CasOptions,
    data: &'a HashMap<String, SecretValue>,
}

#[derive(Debug, Deserialize)]
struct Version {
    version: u32,
}

#[derive(Debug, Deserialize)]
struct ReadBody {
    data: HashMap<String, SecretValue>,
    metadata: Version,
}

pub struct SecretWrite<'a> {
    pub path: &'a KvPath,
    pub data: &'a HashMap<String, SecretValue>,
    pub cas: u32,
}

pub async fn write<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    mount: &Mounts,
    secret: &SecretWrite<'_>,
) -> Result<u32, Error> {
    let guard = Span::new(reporter, "OpenBao write secret", log::ScopeKind::Task).start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Post {
        path: format!("/v1/{mount}/data/{}", secret.path),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&WriteRequest {
            options: CasOptions { cas: secret.cas },
            data: secret.data,
        })?),
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let parsed = serde_json::from_value::<DataWrapper<Version>>(parser.parse(body)?)?;

    guard.finish(Ok(parsed.data.version))
}

pub async fn read<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    mount: &Mounts,
    path: &KvPath,
    version: Option<u32>,
) -> Result<Option<SecretRecord>, Error> {
    let guard = Span::new(reporter, "OpenBao read secret", log::ScopeKind::Task).start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Get {
        path: format!("/v1/{mount}/data/{path}"),
        headers: vec![open_bao_token_header(token)],
        query: version
            .map(|version| HashMap::from([("version".to_string(), version.to_string())]))
            .unwrap_or_default(),
    };

    match rest_client.execute(guard.span(), &req).await? {
        Response::Error { status: 404, .. } => guard.finish(Ok(None)),
        response => {
            let body = assert_okay_with_body(response)?;
            let parsed = serde_json::from_value::<DataWrapper<ReadBody>>(parser.parse(body)?)?;
            guard.finish(Ok(Some(SecretRecord {
                data: parsed.data.data,
                version: parsed.data.metadata.version,
            })))
        }
    }
}

pub async fn list<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    mount: &Mounts,
    path: &KvPath,
) -> Result<Vec<String>, Error> {
    let guard = Span::new(reporter, "OpenBao list secrets", log::ScopeKind::Task).start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Get {
        path: format!("/v1/{mount}/metadata/{path}"),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::from([("list".to_string(), "true".to_string())]),
    };

    match rest_client.execute(guard.span(), &req).await? {
        Response::Error { status: 404, .. } => guard.finish(Ok(Vec::new())),
        response => {
            let body = assert_okay_with_body(response)?;
            let parsed = serde_json::from_value::<DataWrapper<KeysData>>(parser.parse(body)?)?;
            guard.finish(Ok(parsed.data.keys))
        }
    }
}

pub async fn delete<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    mount: &Mounts,
    path: &KvPath,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao delete secret", log::ScopeKind::Task).start_guard();

    if let Err(err) = ensure_key_value(mount) {
        return guard.finish(Err(err));
    }

    let req = Request::Delete {
        path: format!("/v1/{mount}/data/{path}"),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::MockRestClient;

    fn path() -> KvPath {
        KvPath::new("seedlings/hello/DB_PASSWORD").unwrap()
    }

    fn single_value() -> HashMap<String, SecretValue> {
        HashMap::from([("value".to_string(), SecretValue::new("hunter2"))])
    }

    fn okay_with_body(body: &str) -> Response {
        Response::Okay {
            headers: Vec::new(),
            body: Some(body.to_string()),
        }
    }

    fn error_status(status: u16) -> Response {
        Response::Error {
            status,
            headers: Vec::new(),
            body: None,
        }
    }

    fn write_response(version: u32) -> Response {
        okay_with_body(&format!(
            r#"{{"data":{{"created_time":"2026-09-26T00:00:00Z","deletion_time":"","destroyed":false,"version":{version}}}}}"#
        ))
    }

    #[tokio::test]
    async fn write_should_post_the_data_with_the_cas_option_and_return_the_new_version() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/douglas/data/seedlings/hello/DB_PASSWORD"
                            && body.as_deref()
                                == Some(r#"{"options":{"cas":0},"data":{"value":"hunter2"}}"#)
                )
            })
            .returning(|_, _| Ok(write_response(1)));

        let version = write(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &SecretWrite {
                path: &path(),
                data: &single_value(),
                cas: 0,
            },
        )
        .await
        .expect("should write");

        assert_eq!(version, 1);
    }

    #[tokio::test]
    async fn write_should_send_the_current_version_as_cas_when_updating() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { body, .. }
                        if body.as_deref().is_some_and(|body| body.contains(r#""cas":4"#))
                )
            })
            .returning(|_, _| Ok(write_response(5)));

        let version = write(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &SecretWrite {
                path: &path(),
                data: &single_value(),
                cas: 4,
            },
        )
        .await
        .expect("should write");

        assert_eq!(version, 5);
    }

    #[tokio::test]
    async fn write_should_send_the_token_header() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { headers, .. }
                        if headers.contains(&open_bao_token_header("token"))
                )
            })
            .returning(|_, _| Ok(write_response(1)));

        write(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &SecretWrite {
                path: &path(),
                data: &single_value(),
                cas: 0,
            },
        )
        .await
        .expect("should send the token");
    }

    #[tokio::test]
    async fn write_should_fail_when_openbao_rejects_the_check_and_set() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(error_status(400)));

        let result = write(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &SecretWrite {
                path: &path(),
                data: &single_value(),
                cas: 0,
            },
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = write(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::PublicKeyInfrastructure,
            &SecretWrite {
                path: &path(),
                data: &single_value(),
                cas: 0,
            },
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }

    #[tokio::test]
    async fn read_should_get_the_latest_version_and_parse_the_record() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Get { path, query, .. }
                        if path == "/v1/douglas/data/seedlings/hello/DB_PASSWORD"
                            && query.is_empty()
                )
            })
            .returning(|_, _| {
                Ok(okay_with_body(
                    r#"{"data":{"data":{"value":"hunter2"},"metadata":{"version":3,"destroyed":false}}}"#,
                ))
            });

        let record = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &path(),
            None,
        )
        .await
        .expect("should read")
        .expect("should exist");

        assert_eq!(record.version, 3);
        assert_eq!(record.data, single_value());
    }

    #[tokio::test]
    async fn read_should_request_a_specific_version_when_asked() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Get { query, .. }
                        if query.get("version").map(String::as_str) == Some("2")
                )
            })
            .returning(|_, _| {
                Ok(okay_with_body(
                    r#"{"data":{"data":{"value":"old"},"metadata":{"version":2}}}"#,
                ))
            });

        let record = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &path(),
            Some(2),
        )
        .await
        .expect("should read")
        .expect("should exist");

        assert_eq!(record.version, 2);
    }

    #[tokio::test]
    async fn read_should_return_none_when_the_secret_does_not_exist() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(error_status(404)));

        let record = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &path(),
            None,
        )
        .await
        .expect("a missing secret is not an error");

        assert_eq!(record, None);
    }

    #[tokio::test]
    async fn read_should_fail_on_an_error_other_than_not_found() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(error_status(403)));

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &path(),
            None,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_should_fail_when_the_body_is_not_a_secret() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(okay_with_body(r#"{"data":{"unexpected":true}}"#)));

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &path(),
            None,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = read(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::PublicKeyInfrastructure,
            &path(),
            None,
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }

    #[tokio::test]
    async fn list_should_get_the_metadata_path_with_the_list_flag_and_return_the_keys() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Get { path, query, .. }
                        if path == "/v1/douglas/metadata/seedlings/hello"
                            && query.get("list").map(String::as_str) == Some("true")
                )
            })
            .returning(|_, _| {
                Ok(okay_with_body(
                    r#"{"data":{"keys":["API_TOKEN","DB_PASSWORD"]}}"#,
                ))
            });

        let keys = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &KvPath::new("seedlings/hello").unwrap(),
        )
        .await
        .expect("should list");

        assert_eq!(keys, vec!["API_TOKEN", "DB_PASSWORD"]);
    }

    #[tokio::test]
    async fn list_should_return_no_keys_when_the_path_does_not_exist() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(error_status(404)));

        let keys = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::ManagedSecrets,
            &KvPath::new("seedlings/none").unwrap(),
        )
        .await
        .expect("a missing path is not an error");

        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn list_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "token",
            &Mounts::PublicKeyInfrastructure,
            &path(),
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }

    #[tokio::test]
    async fn delete_should_delete_the_data_path() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Delete { path, headers, .. }
                        if path == "/v1/douglas/data/seedlings/hello/DB_PASSWORD"
                            && headers.contains(&open_bao_token_header("token"))
                )
            })
            .returning(|_, _| {
                Ok(Response::NoContent {
                    headers: Vec::new(),
                })
            });

        delete(
            Arc::new(NullReporter),
            &mut rest_client,
            "token",
            &Mounts::ManagedSecrets,
            &path(),
        )
        .await
        .expect("should delete");
    }

    #[tokio::test]
    async fn delete_should_fail_when_openbao_rejects_the_request() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .returning(|_, _| Ok(error_status(403)));

        let result = delete(
            Arc::new(NullReporter),
            &mut rest_client,
            "token",
            &Mounts::ManagedSecrets,
            &path(),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_should_refuse_a_mount_that_is_not_a_key_value_mount() {
        let mut rest_client = MockRestClient::new();

        let result = delete(
            Arc::new(NullReporter),
            &mut rest_client,
            "token",
            &Mounts::PublicKeyInfrastructure,
            &path(),
        )
        .await;

        assert!(matches!(result, Err(Error::Error(_))));
    }
}
