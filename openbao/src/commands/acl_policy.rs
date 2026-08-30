use crate::{
    Error,
    commands::{DataWrapper, KeysData, open_bao_token_header},
};
use log::{Reporter, Span};
use openbao_types::Capability;
use serde_json::json;
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{AssertionError, assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

fn to_policy_document(policies: &HashMap<String, HashSet<Capability>>) -> serde_json::Value {
    let paths = policies
        .iter()
        .map(|(path, capabilities)| {
            let capabilities = capabilities
                .iter()
                .map(|capability| capability.to_string())
                .collect::<Vec<String>>();
            (path.clone(), json!({ "capabilities": capabilities }))
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();

    json!({ "path": paths })
}

pub async fn upsert<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    name: &str,
    policies: &HashMap<String, HashSet<Capability>>,
) -> Result<(), Error> {
    let guard =
        Span::new(reporter, "OpenBao upsert ACL policy", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: format!("/v1/sys/policies/acl/{name}"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&json!({
            "policy": to_policy_document(policies).to_string(),
        }))?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn delete<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    name: &str,
) -> Result<(), Error> {
    let guard =
        Span::new(reporter, "OpenBao delete ACL policy", log::ScopeKind::Task).start_guard();

    let req = Request::Delete {
        path: format!("/v1/sys/policies/acl/{name}"),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn list<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
) -> Result<Vec<String>, Error> {
    let guard =
        Span::new(reporter, "OpenBao list ACL policies", log::ScopeKind::Task).start_guard();
    let req = Request::Get {
        path: "/v1/sys/policies/acl".to_string(),
        headers: vec![open_bao_token_header(token)],
        query: HashMap::from([("list".to_string(), "true".to_string())]),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = match assert_okay_with_body(response) {
        Ok(body) => body,
        Err(AssertionError::NotFoundError) => return guard.finish(Ok(Vec::new())),
        Err(err) => return guard.finish(Err(err.into())),
    };

    let json = parser.parse(body)?;
    let parsed = serde_json::from_value::<DataWrapper<KeysData>>(json)?;

    guard.finish(Ok(parsed.data.keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::{MockRestClient, Response};

    #[test]
    fn to_policy_document_should_nest_capabilities_under_each_path() {
        let policies =
            HashMap::from([("sys/mounts".to_string(), HashSet::from([Capability::Read]))]);

        assert_eq!(
            to_policy_document(&policies),
            json!({ "path": { "sys/mounts": { "capabilities": ["read"] } } })
        );
    }

    #[tokio::test]
    async fn execute_should_post_the_policy_document_as_a_string_field() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/sys/policies/acl/douglas-admin"
                            && body.as_deref()
                                == Some(r#"{"policy":"{\"path\":{\"sys/mounts\":{\"capabilities\":[\"read\"]}}}"}"#)
                )
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        let policies =
            HashMap::from([("sys/mounts".to_string(), HashSet::from([Capability::Read]))]);

        upsert(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "douglas-admin",
            &policies,
        )
        .await
        .expect("should upsert the policy");
    }

    #[tokio::test]
    async fn delete_should_send_a_delete_to_the_policys_own_path() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Delete { path, .. } if path == "/v1/sys/policies/acl/seedling.hello-openbao")
            })
            .returning(|_, _| Ok(Response::NoContent { headers: Vec::new() }));

        delete(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            "seedling.hello-openbao",
        )
        .await
        .expect("should delete the policy");
    }

    #[tokio::test]
    async fn list_should_return_the_parsed_keys() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, query, .. } if path == "/v1/sys/policies/acl" && query.get("list") == Some(&"true".to_string()))
            })
            .returning(|_, _| {
                Ok(Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"keys":["seedling.hello-openbao"]}}"#.to_string()),
                })
            });

        let policies = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should list policies");

        assert_eq!(policies, vec!["seedling.hello-openbao".to_string()]);
    }

    #[tokio::test]
    async fn list_should_return_an_empty_list_when_no_policies_exist_yet() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(Response::Error {
                headers: Vec::new(),
                status: 404,
                body: None,
            })
        });

        let policies = list(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should treat a 404 as no policies");

        assert!(policies.is_empty());
    }
}
