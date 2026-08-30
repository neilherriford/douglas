use crate::{
    Error,
    commands::{DataWrapper, open_bao_token_header},
};
use log::{Reporter, Span};
use openbao_types::Mounts;
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

pub async fn list<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
) -> Result<HashMap<String, String>, Error> {
    let guard = Span::new(reporter, "List mounts", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: "/v1/sys/mounts".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;

    let parsed = serde_json::from_value::<DataWrapper<HashMap<String, MountDetail>>>(json)?;
    let result = parsed
        .data
        .iter()
        .map(|(name, mount_detail)| (name.to_string(), mount_detail.kind.clone()))
        .collect::<HashMap<String, String>>();

    guard.finish(Ok(result))
}

#[derive(Debug, Deserialize)]
struct MountDetail {
    #[serde(rename = "type")]
    kind: String,
}

pub async fn create<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    mount: Mounts,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "Mount", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: format!("/v1/sys/mounts/{mount}"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&MountRequest {
            mount: mount.clone(),
            description: create_description(&mount),
            options: create_options(&mount),
            config: create_config(&mount),
        })?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

fn create_config(mount: &Mounts) -> Option<Config> {
    match mount {
        Mounts::KeyValueStore => None,
        Mounts::PublicKeyInfrastructure => Some(Config {
            max_lease_ttl: Some("87600h".to_string()),
        }),
    }
}

fn create_options(mount: &Mounts) -> Option<HashMap<String, String>> {
    if mount != &Mounts::KeyValueStore {
        return None;
    }

    Some(HashMap::from([("version".to_string(), "2".to_string())]))
}

fn create_description(mount: &Mounts) -> String {
    match mount {
        Mounts::KeyValueStore => "Douglas key value store",
        Mounts::PublicKeyInfrastructure => "Douglas public key infrastructure",
    }
    .to_string()
}

#[derive(Debug, Serialize)]
struct MountRequest {
    #[serde(rename = "type")]
    mount: Mounts,
    description: String,
    config: Option<Config>,
    options: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct Config {
    max_lease_ttl: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};
    use std::sync::Arc;

    #[test]
    fn create_config_should_set_a_long_lease_ttl_only_for_pki() {
        assert!(create_config(&Mounts::KeyValueStore).is_none());
        assert_eq!(
            create_config(&Mounts::PublicKeyInfrastructure)
                .unwrap()
                .max_lease_ttl,
            Some("87600h".to_string())
        );
    }

    #[test]
    fn create_options_should_set_kv_version_two_only_for_the_key_value_store() {
        assert_eq!(
            create_options(&Mounts::KeyValueStore),
            Some(HashMap::from([("version".to_string(), "2".to_string())]))
        );
        assert_eq!(create_options(&Mounts::PublicKeyInfrastructure), None);
    }

    #[tokio::test]
    async fn list_mounts_should_map_each_path_to_its_backend_type() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(
                |_, request| matches!(request, Request::Get { path, .. } if path == "/v1/sys/mounts"),
            )
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(
                        r#"{"data":{"kv/":{"type":"kv"},"pki/":{"type":"pki"}}}"#.to_string(),
                    ),
                })
            });

        let mounts = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should list mounts");

        assert_eq!(
            mounts,
            HashMap::from([
                ("kv/".to_string(), "kv".to_string()),
                ("pki/".to_string(), "pki".to_string()),
            ])
        );
    }

    #[tokio::test]
    async fn mount_should_post_a_kv_v2_mount_request_for_the_key_value_store() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/sys/mounts/kv"
                            && body.as_deref()
                                == Some(r#"{"type":"kv","description":"Douglas key value store","config":null,"options":{"version":"2"}}"#)
                )
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        create(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            Mounts::KeyValueStore,
        )
        .await
        .expect("should mount the key value store");
    }
}
