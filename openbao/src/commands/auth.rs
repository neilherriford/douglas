use crate::{
    Error,
    commands::{DataWrapper, KeysData, open_bao_token_header, utils::headers::vault_token},
};
use log::{Reporter, Span};
use openbao_types::{AuthType, Period};
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{AssertionError, assert_okay_or_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
use std::{collections::HashMap, sync::Arc};

pub async fn exists<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
) -> Result<bool, Error> {
    let guard = Span::new(reporter, "OpenBao role exists", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: format!("/v1/auth/{auth_type}/role/{name}/role-id"),
        headers: vec![vault_token(token)],
        query: HashMap::new(),
    };

    guard.finish(match rest_client.execute(guard.span(), &req).await? {
        simple_rest_client::Response::Okay { .. } => Ok(true),
        simple_rest_client::Response::Created { body, .. } => Err(Error::UnexpectedResponse {
            status: 201,
            body,
            message: "expected OK, but received CREATED".to_string(),
        }),
        simple_rest_client::Response::NoContent { .. } => Err(Error::UnexpectedResponse {
            status: 204,
            body: None,
            message: "expected OK, but received NO CONTENT".to_string(),
        }),
        simple_rest_client::Response::Error { status: 404, .. } => Ok(false),
        simple_rest_client::Response::Error { status, body, .. } => {
            Err(Error::UnexpectedResponse {
                status,
                body,
                message: "unexpected error".to_string(),
            })
        }
    })
}

#[derive(Debug, PartialEq, Serialize)]
struct CreateRoleBody {
    token_policies: Vec<String>,
    token_ttl: Period,
    token_max_ttl: Period,
    secret_id_ttl: Period,
    secret_id_num_uses: usize,
    bind_secret_id: bool,
}

pub async fn create<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
    policy_names: Vec<String>,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao create role", log::ScopeKind::Task).start_guard();
    let req = Request::Post {
        path: format!("/v1/auth/{auth_type}/role/{name}"),
        headers: vec![vault_token(token), Header::content_type_json()],
        body: Some(serde_json::to_string(&CreateRoleBody {
            token_policies: policy_names,
            token_ttl: Period::Hours(1),
            token_max_ttl: Period::Hours(4),
            secret_id_ttl: Period::Hours(0),
            secret_id_num_uses: 0,
            bind_secret_id: true,
        })?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

#[derive(Debug, PartialEq, Deserialize)]
struct RoleIdData {
    role_id: String,
}

pub async fn get_role_id<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
) -> Result<String, Error> {
    let guard = Span::new(reporter, "OpenBao get role id", log::ScopeKind::Task).start_guard();
    let req = Request::Get {
        path: format!("/v1/auth/{auth_type}/role/{name}/role-id"),
        headers: vec![vault_token(token)],
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;
    let parsed = serde_json::from_value::<DataWrapper<RoleIdData>>(json)?;

    guard.finish(Ok(parsed.data.role_id))
}

pub async fn list_roles<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
) -> Result<Vec<String>, Error> {
    let guard = Span::new(reporter, "OpenBao list roles", log::ScopeKind::Task).start_guard();
    let req = Request::Get {
        path: format!("/v1/auth/{auth_type}/role"),
        headers: vec![vault_token(token)],
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

#[derive(Debug, PartialEq, Deserialize)]
struct SecretData {
    secret_id: String,
}

pub async fn create_secret<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
) -> Result<String, Error> {
    let guard = Span::new(reporter, "OpenBao create secret", log::ScopeKind::Task).start_guard();
    let req = Request::Post {
        path: format!("/v1/auth/{auth_type}/role/{name}/secret-id"),
        headers: vec![vault_token(token)],
        body: None,
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;
    let parsed = serde_json::from_value::<DataWrapper<SecretData>>(json)?;

    guard.finish(Ok(parsed.data.secret_id))
}

pub async fn list_secret_id_accessors<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
) -> Result<Vec<String>, Error> {
    let guard = Span::new(
        reporter,
        "OpenBao list secret id accessors",
        log::ScopeKind::Task,
    )
    .start_guard();
    let req = Request::Get {
        path: format!("/v1/auth/{auth_type}/role/{name}/secret-id"),
        headers: vec![vault_token(token)],
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

pub async fn destroy_secret_id_accessor<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
    accessor: &'a str,
) -> Result<(), Error> {
    let guard = Span::new(
        reporter,
        "OpenBao destroy secret id accessor",
        log::ScopeKind::Task,
    )
    .start_guard();
    let req = Request::Post {
        path: format!("/v1/auth/{auth_type}/role/{name}/secret-id-accessor/destroy"),
        headers: vec![Header::content_type_json(), vault_token(token)],
        body: Some(serde_json::to_string(&serde_json::json!({
            "secret_id_accessor": accessor,
        }))?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn delete_role<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao delete role", log::ScopeKind::Task).start_guard();
    let req = Request::Delete {
        path: format!("/v1/auth/{auth_type}/role/{name}"),
        headers: vec![vault_token(token)],
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

pub async fn revoke<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
) -> Result<(), Error> {
    let guard = Span::new(reporter, "OpenBao revoke token", log::ScopeKind::Task).start_guard();
    let req = Request::Post {
        path: "/v1/auth/token/revoke-self".into(),
        headers: vec![vault_token(token)],
        body: None,
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AuthDetail {
    #[serde(rename = "type")]
    kind: String,
}

pub async fn list_auth_methods<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
) -> Result<HashMap<String, String>, Error> {
    let guard =
        Span::new(reporter, "OpenBao list auth methods", log::ScopeKind::Task).start_guard();

    let req = Request::Get {
        path: "/v1/sys/auth".to_string(),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        query: HashMap::new(),
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body)?;

    let parsed = serde_json::from_value::<DataWrapper<HashMap<String, AuthDetail>>>(json)?;
    let result = parsed
        .data
        .iter()
        .map(|(name, mount_detail)| (name.to_string(), mount_detail.kind.clone()))
        .collect::<HashMap<String, String>>();

    guard.finish(Ok(result))
}

pub async fn enable_auth_method<'a>(
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &AuthType,
) -> Result<(), Error> {
    let guard =
        Span::new(reporter, "OpenBao enable auth method", log::ScopeKind::Task).start_guard();

    let req = Request::Post {
        path: format!("/v1/sys/auth/{auth_type}"),
        headers: vec![Header::content_type_json(), open_bao_token_header(token)],
        body: Some(serde_json::to_string(&EnableAuthMethodRequest {
            auth_type: auth_type.clone(),
        })?),
        query: HashMap::new(),
    };

    guard.finish(assert_okay_or_no_content(
        rest_client.execute(guard.span(), &req).await?,
    ))?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct EnableAuthMethodRequest {
    #[serde(rename = "type")]
    auth_type: AuthType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::NullReporter;
    use simple_rest_client::MockRestClient;
    use std::sync::Arc;

    #[tokio::test]
    async fn exists_should_return_true_when_the_role_id_is_found() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, .. } if path == "/v1/auth/approle/role/douglas.cli/role-id")
            })
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"role_id":"role-1"}}"#.to_string()),
                })
            });

        let result = exists(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
            "douglas.cli",
        )
        .await
        .expect("should check existence");

        assert!(result);
    }

    #[tokio::test]
    async fn exists_should_return_false_on_a_404() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(simple_rest_client::Response::Error {
                headers: Vec::new(),
                status: 404,
                body: None,
            })
        });

        let result = exists(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
            "douglas.cli",
        )
        .await
        .expect("should check existence");

        assert!(!result);
    }

    #[tokio::test]
    async fn create_should_post_the_role_referencing_only_the_given_policy_names() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/auth/approle/role/douglas.cli"
                            && body.as_deref()
                                == Some(r#"{"token_policies":["douglas-admin"],"token_ttl":"1h","token_max_ttl":"4h","secret_id_ttl":"0h","secret_id_num_uses":0,"bind_secret_id":true}"#)
                )
            })
            .returning(|_, _| Ok(simple_rest_client::Response::NoContent { headers: Vec::new() }));

        create(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
            "douglas.cli",
            vec!["douglas-admin".to_string()],
        )
        .await
        .expect("should create the role");
    }

    #[tokio::test]
    async fn get_role_id_should_return_the_parsed_role_id() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, .. } if path == "/v1/auth/approle/role/douglas.cli/role-id")
            })
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"role_id":"role-1"}}"#.to_string()),
                })
            });

        let role_id = get_role_id(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
            "douglas.cli",
        )
        .await
        .expect("should fetch the role id");

        assert_eq!(role_id, "role-1");
    }

    #[tokio::test]
    async fn create_secret_should_post_to_the_secret_id_endpoint_and_return_it() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Post { path, body, .. } if path == "/v1/auth/approle/role/douglas.cli/secret-id" && body.is_none())
            })
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"secret_id":"secret-1"}}"#.to_string()),
                })
            });

        let secret_id = create_secret(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
            "douglas.cli",
        )
        .await
        .expect("should mint a secret id");

        assert_eq!(secret_id, "secret-1");
    }

    #[tokio::test]
    async fn list_secret_id_accessors_should_return_the_parsed_keys() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, query, .. } if path == "/v1/auth/approle/role/seedling.hello-openbao/secret-id" && query.get("list") == Some(&"true".to_string()))
            })
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"keys":["accessor-1"]}}"#.to_string()),
                })
            });

        let accessors = list_secret_id_accessors(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
            "seedling.hello-openbao",
        )
        .await
        .expect("should list secret id accessors");

        assert_eq!(accessors, vec!["accessor-1".to_string()]);
    }

    #[tokio::test]
    async fn list_secret_id_accessors_should_return_an_empty_list_when_none_exist_yet() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(simple_rest_client::Response::Error {
                headers: Vec::new(),
                status: 404,
                body: None,
            })
        });

        let accessors = list_secret_id_accessors(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
            "seedling.hello-openbao",
        )
        .await
        .expect("should treat a 404 as no accessors");

        assert!(accessors.is_empty());
    }

    #[tokio::test]
    async fn destroy_secret_id_accessor_should_post_the_accessor_to_the_destroy_endpoint() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/auth/approle/role/seedling.hello-openbao/secret-id-accessor/destroy"
                            && body.as_deref() == Some(r#"{"secret_id_accessor":"accessor-1"}"#)
                )
            })
            .returning(|_, _| Ok(simple_rest_client::Response::NoContent { headers: Vec::new() }));

        destroy_secret_id_accessor(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
            "seedling.hello-openbao",
            "accessor-1",
        )
        .await
        .expect("should destroy the secret id accessor");
    }

    #[tokio::test]
    async fn delete_role_should_send_a_delete_to_the_roles_own_path() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Delete { path, .. } if path == "/v1/auth/approle/role/seedling.hello-openbao")
            })
            .returning(|_, _| Ok(simple_rest_client::Response::NoContent { headers: Vec::new() }));

        delete_role(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
            "seedling.hello-openbao",
        )
        .await
        .expect("should delete the role");
    }

    #[tokio::test]
    async fn list_roles_should_return_the_parsed_keys() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Get { path, query, .. } if path == "/v1/auth/approle/role" && query.get("list") == Some(&"true".to_string()))
            })
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"keys":["seedling.hello-openbao"]}}"#.to_string()),
                })
            });

        let roles = list_roles(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
        )
        .await
        .expect("should list roles");

        assert_eq!(roles, vec!["seedling.hello-openbao".to_string()]);
    }

    #[tokio::test]
    async fn list_roles_should_return_an_empty_list_when_no_roles_exist_yet() {
        let mut rest_client = MockRestClient::new();
        rest_client.expect_execute().returning(|_, _| {
            Ok(simple_rest_client::Response::Error {
                headers: Vec::new(),
                status: 404,
                body: None,
            })
        });

        let roles = list_roles(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
            &AuthType::AppRole,
        )
        .await
        .expect("should treat a 404 as no roles");

        assert!(roles.is_empty());
    }

    #[tokio::test]
    async fn revoke_should_post_to_the_revoke_self_endpoint() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(request, Request::Post { path, .. } if path == "/v1/auth/token/revoke-self")
            })
            .returning(|_, _| Ok(simple_rest_client::Response::NoContent { headers: Vec::new() }));

        revoke(Arc::new(NullReporter), &mut rest_client, "root-token")
            .await
            .expect("should revoke the token");
    }

    #[tokio::test]
    async fn list_auth_methods_should_map_each_path_to_its_backend_type() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(
                |_, request| matches!(request, Request::Get { path, .. } if path == "/v1/sys/auth"),
            )
            .returning(|_, _| {
                Ok(simple_rest_client::Response::Okay {
                    headers: Vec::new(),
                    body: Some(r#"{"data":{"approle/":{"type":"approle"}}}"#.to_string()),
                })
            });

        let methods = list_auth_methods(
            Arc::new(NullReporter),
            &mut rest_client,
            &JsonParser::new(),
            "root-token",
        )
        .await
        .expect("should list auth methods");

        assert_eq!(
            methods,
            HashMap::from([("approle/".to_string(), "approle".to_string())])
        );
    }

    #[tokio::test]
    async fn enable_auth_method_should_post_the_auth_type_as_the_backend_type() {
        let mut rest_client = MockRestClient::new();
        rest_client
            .expect_execute()
            .withf(|_, request| {
                matches!(
                    request,
                    Request::Post { path, body, .. }
                        if path == "/v1/sys/auth/approle" && body.as_deref() == Some(r#"{"type":"approle"}"#)
                )
            })
            .returning(|_, _| Ok(simple_rest_client::Response::NoContent { headers: Vec::new() }));

        enable_auth_method(
            Arc::new(NullReporter),
            &mut rest_client,
            "root-token",
            &AuthType::AppRole,
        )
        .await
        .expect("should enable the auth method");
    }
}
