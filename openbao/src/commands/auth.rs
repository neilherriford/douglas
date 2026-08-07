use std::sync::Arc;

use crate::{AuthType, OpenBaoError, Period, RoleId, commands::utils::headers::vault_token};
use log::{Reporter, Span};
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::{assert_no_content, assert_okay_with_body},
    parsers::{Parser, json::JsonParser},
};
impl AuthType {
    pub fn mount_name(&self) -> String {
        match self {
            AuthType::AppRole => "approle".into(),
        }
    }
}

impl Serialize for AuthType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AuthType::AppRole => serializer.serialize_str("approle"),
        }
    }
}

pub struct IsInstalledCommand<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
}

impl<'a> IsInstalledCommand<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        token: &'a str,
        auth_type: &'a AuthType,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            token,
            auth_type,
        }
    }

    pub async fn perform(&mut self) -> Result<bool, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao is initialized",
            log::ScopeKind::Task,
        )
        .start_guard();

        let req = Request::Get {
            path: format!("/v1/sys/auth/{}", self.auth_type.mount_name()),
            headers: vec![vault_token(self.token)],
        };

        guard.finish(match self.rest_client.execute(guard.span(), &req).await? {
            simple_rest_client::Response::Okay { .. } => Ok(true),
            simple_rest_client::Response::Created { body, .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 201,
                    body,
                    message: "expected OK, but received CREATED".to_string(),
                })
            }
            simple_rest_client::Response::NoContent { .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 204,
                    body: None,
                    message: "expected OK, but received NO CONTENT".to_string(),
                })
            }
            simple_rest_client::Response::Error { status: 400, .. } => Ok(false),
            simple_rest_client::Response::Error { body, .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 204,
                    body,
                    message: "unexpected error".to_string(),
                })
            }
        })
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct AuthRequest {
    #[serde(rename = "type")]
    auth_type: AuthType,
    description: String,
}

pub struct InstallAuthCommand<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    description: &'a str,
}

impl<'a> InstallAuthCommand<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        token: &'a str,
        auth_type: &'a AuthType,
        description: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            token,
            auth_type,
            description,
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao install auth",
            log::ScopeKind::Task,
        )
        .start_guard();

        let req = Request::Post {
            path: format!("/v1/sys/auth/{}", self.auth_type.mount_name()),
            headers: vec![vault_token(self.token), Header::content_type_json()],
            body: Some(serde_json::to_string(&AuthRequest {
                auth_type: self.auth_type.clone(),
                description: self.description.to_string(),
            })?),
        };

        guard.finish(assert_no_content(
            self.rest_client.execute(guard.span(), &req).await?,
        ))?;
        Ok(())
    }
}

pub struct RoleExists<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
}

impl<'a> RoleExists<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        token: &'a str,
        auth_type: &'a AuthType,
        name: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            token,
            auth_type,
            name,
        }
    }

    pub async fn perform(&mut self) -> Result<bool, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao role exists",
            log::ScopeKind::Task,
        )
        .start_guard();

        let req = Request::Get {
            path: format!(
                "/v1/auth/{}/role/{}/role-id",
                self.auth_type.mount_name(),
                self.name
            ),
            headers: vec![vault_token(self.token)],
        };

        guard.finish(match self.rest_client.execute(guard.span(), &req).await? {
            simple_rest_client::Response::Okay { .. } => Ok(true),
            simple_rest_client::Response::Created { body, .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 201,
                    body,
                    message: "expected OK, but received CREATED".to_string(),
                })
            }
            simple_rest_client::Response::NoContent { .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 204,
                    body: None,
                    message: "expected OK, but received NO CONTENT".to_string(),
                })
            }
            simple_rest_client::Response::Error { status: 404, .. } => Ok(false),
            simple_rest_client::Response::Error { status, body, .. } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status,
                    body,
                    message: "unexpected error".to_string(),
                })
            }
        })
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct CreateRoleBody {
    policies: Vec<String>,
    token_ttl: Period,
    token_max_ttl: Period,
    secret_id_ttl: Period,
    secret_id_num_uses: usize,
    bind_secret_id: bool,
}

#[derive(Debug, Deserialize)]
struct DataWrapper<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct AuthWrapper<T> {
    auth: T,
}

#[derive(Debug, Deserialize)]
struct RoleIdData {
    role_id: String,
}

pub struct CreateRole<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
    policies: Vec<String>,
}

impl<'a> CreateRole<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        token: &'a str,
        auth_type: &'a AuthType,
        name: &'a str,
        policies: Vec<&str>,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            token,
            auth_type,
            name,
            policies: policies.iter().map(|p| p.to_string()).collect(),
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao create role",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: format!(
                "/v1/auth/{}/role/{}",
                self.auth_type.mount_name(),
                self.name
            ),
            headers: vec![vault_token(self.token), Header::content_type_json()],
            body: Some(serde_json::to_string(&CreateRoleBody {
                policies: self.policies.clone(),
                token_ttl: Period::Hours(1),
                token_max_ttl: Period::Hours(4),
                secret_id_ttl: Period::Hours(0),
                secret_id_num_uses: 0,
                bind_secret_id: true,
            })?),
        };

        guard.finish(assert_no_content(
            self.rest_client.execute(guard.span(), &req).await?,
        ))?;
        Ok(())
    }
}

pub struct GetRoleId<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
}

impl<'a> GetRoleId<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        token: &'a str,
        auth_type: &'a AuthType,
        name: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
            token,
            auth_type,
            name,
        }
    }

    pub async fn perform(&mut self) -> Result<RoleId, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao get role id",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Get {
            path: format!(
                "/v1/auth/{}/role/{}/role-id",
                self.auth_type.mount_name(),
                self.name
            ),
            headers: vec![vault_token(self.token)],
        };

        let response = self.rest_client.execute(guard.span(), &req).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        let parsed = serde_json::from_value::<DataWrapper<RoleIdData>>(json)?;

        guard.finish(Ok(RoleId(parsed.data.role_id)))
    }
}

#[derive(Debug, PartialEq, Deserialize)]
struct SecretData {
    secret_id: String,
}

pub struct CreateSecret<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    token: &'a str,
    auth_type: &'a AuthType,
    name: &'a str,
}

impl<'a> CreateSecret<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        token: &'a str,
        auth_type: &'a AuthType,
        name: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
            token,
            auth_type,
            name,
        }
    }

    pub async fn perform(&mut self) -> Result<String, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao create secret",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: format!(
                "/v1/auth/{}/role/{}/secret-id",
                self.auth_type.mount_name(),
                self.name
            ),
            headers: vec![vault_token(self.token)],
            body: None,
        };

        let response = self.rest_client.execute(guard.span(), &req).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        let parsed = serde_json::from_value::<DataWrapper<SecretData>>(json)?;

        guard.finish(Ok(parsed.data.secret_id))
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct LoginRequest {
    role_id: String,
    secret_id: String,
}

#[derive(Debug, PartialEq, Deserialize)]
struct LoginData {
    client_token: String,
}

pub struct Login<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    auth_type: &'a AuthType,
    name: &'a str,
    secret: &'a str,
}

impl<'a> Login<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        auth_type: &'a AuthType,
        name: &'a str,
        secret: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
            auth_type,
            name,
            secret,
        }
    }

    pub async fn perform(&mut self) -> Result<String, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao log in",
            log::ScopeKind::Task,
        )
        .start_guard();

        let req = Request::Post {
            path: format!("/v1/auth/{}/login", self.auth_type.mount_name()),
            headers: vec![Header::content_type_json()],
            body: Some(serde_json::to_string(&LoginRequest {
                role_id: self.name.into(),
                secret_id: self.secret.into(),
            })?),
        };

        let response = self.rest_client.execute(guard.span(), &req).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;
        let parsed = serde_json::from_value::<AuthWrapper<LoginData>>(json)?;

        guard.finish(Ok(parsed.auth.client_token))
    }
}

pub struct RevokeToken<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    token: &'a str,
}

impl<'a> RevokeToken<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        token: &'a str,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            token,
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao revoke token",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: "/v1/auth/token/revoke-self".into(),
            headers: vec![vault_token(self.token)],
            body: None,
        };

        guard.finish(assert_no_content(
            self.rest_client.execute(guard.span(), &req).await?,
        ))?;
        Ok(())
    }
}
