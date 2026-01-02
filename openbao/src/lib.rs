mod commands;

use crate::commands::{
    init::{ConfigError, InitCommand},
    status::StatusCommand,
    unseal::UnsealCommand,
    upsert_acl_policy::UpsertAclPolicy,
};
use async_trait::async_trait;
use log::Logger;
use serde::{Deserialize, Serialize};
use simple_rest_client::{
    RestClient, RestClientError,
    assertions::AssertionError,
    parsers::json::{JsonParser, JsonParserError},
    unix_domain_socket::{BuilderError, build_client},
};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

pub mod policy {
    pub use crate::commands::upsert_acl_policy::{Capability, Path, Policy};
}

#[derive(Debug, PartialEq)]
pub enum Period {
    Hours(usize),
}

impl Serialize for Period {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Period::Hours(amount) => serializer.serialize_str(&format!("{amount}h")),
        }
    }
}

#[derive(Error, Debug)]
pub enum OpenBaoError {
    #[error("Unknown role: '(0)'")]
    UnknownRole(String),

    #[error("Already initialized")]
    AlreadyInitialized,

    #[error("Insufficient Secrets")]
    InsufficentSecrets,

    #[error("Unseal error: {0:?}")]
    UnsealError(Vec<String>),

    #[error("Received unexpected response with status: {status}, {message}")]
    UnexpectedResponse {
        status: u16,
        body: Option<String>,
        message: String,
    },

    #[error("Client error: {0}")]
    ClientError(#[from] RestClientError),

    #[error("Client error: {0}")]
    ClientResponseError(#[from] AssertionError),

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("Init error: {0}")]
    InitError(#[from] BuilderError),

    #[error("Configuration error: {0}")]
    ConfigurationError(#[from] ConfigError),

    #[error("General Error")]
    Error(String),
}

impl From<serde_json::Error> for OpenBaoError {
    fn from(err: serde_json::Error) -> OpenBaoError {
        OpenBaoError::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

impl From<JsonParserError> for OpenBaoError {
    fn from(err: JsonParserError) -> OpenBaoError {
        match err {
            JsonParserError::Error {
                line,
                column,
                message,
            } => OpenBaoError::ParseError {
                line,
                column,
                message,
            },
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub enum ReplicationMode {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "primary")]
    Primary,
    #[serde(rename = "secondary")]
    Secondary,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Status {
    pub initialized: bool,
    pub sealed: bool,
    pub standby: bool,
    pub performance_standby: bool,
    pub replication_performance_mode: ReplicationMode,
    pub replication_dr_mode: ReplicationMode,
    pub server_time_utc: u32,
    pub version: String,
}

#[derive(Debug)]
pub struct Secrets {
    pub secrets: Vec<Secret>,
    pub root_token: String,
}

#[derive(Debug, Clone)]
pub struct Secret {
    pub key: String,
    pub base64: String,
}

#[derive(Debug, PartialEq, Clone)]
pub enum AuthType {
    AppRole,
}

#[derive(Debug, PartialEq, Clone)]
pub struct RoleId(String);

impl std::fmt::Display for RoleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Display for AuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthType::AppRole => f.write_str("approle"),
        }
    }
}

#[async_trait]
pub trait SystemClient {
    async fn status(&mut self) -> Result<Status, OpenBaoError>;
    async fn intialize(&mut self) -> Result<Secrets, OpenBaoError>;
    async fn unseal(&mut self, secrets: &Secrets) -> Result<(), OpenBaoError>;
}

pub struct SimpleSystemClient {
    rest_client: Box<dyn RestClient>,
    parser: JsonParser,
    logger: Arc<dyn Logger>,
}

impl SimpleSystemClient {
    pub async fn build(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
    ) -> Result<Self, OpenBaoError> {
        let rest_client = build_client(socket_file_path, Arc::clone(&logger)).await?;

        Ok(Self {
            rest_client: Box::new(rest_client),
            parser: JsonParser::new(),
            logger,
        })
    }
}

#[async_trait]
impl SystemClient for SimpleSystemClient {
    async fn status(&mut self) -> Result<Status, OpenBaoError> {
        Ok(StatusCommand::new(self.rest_client.as_mut(), &self.parser)
            .perform()
            .await?)
    }

    async fn intialize(&mut self) -> Result<Secrets, OpenBaoError> {
        Ok(InitCommand::new(
            self.rest_client.as_mut(),
            &self.parser,
            &commands::init::Config::new(10, 3)?,
        )
        .perform()
        .await?)
    }

    async fn unseal(&mut self, secrets: &Secrets) -> Result<(), OpenBaoError> {
        Ok(UnsealCommand::new(
            self.rest_client.as_mut(),
            &self.parser,
            &secrets.secrets,
            Arc::clone(&self.logger),
        )
        .perform()
        .await?)
    }
}

#[async_trait]
pub trait AuthenticatedClient {
    async fn upsert_acl_policy(
        &mut self,
        name: &str,
        policies: &[policy::Policy],
    ) -> Result<(), OpenBaoError>;
    async fn has_auth(&mut self, auth_type: AuthType) -> Result<bool, OpenBaoError>;
    async fn install_auth(
        &mut self,
        auth_type: AuthType,
        description: &str,
    ) -> Result<(), OpenBaoError>;
    async fn create_app_role(
        &mut self,
        auth_type: AuthType,
        name: &str,
        policies: Vec<&str>,
    ) -> Result<RoleId, OpenBaoError>;
    async fn create_app_role_secret(
        &mut self,
        auth_type: AuthType,
        name: &str,
    ) -> Result<String, OpenBaoError>;
    async fn revoke_token(&mut self, token: &str) -> Result<(), OpenBaoError>;
}

pub struct SimpleAuthenticatedClient {
    rest_client: Box<dyn RestClient>,
    parser: JsonParser,
    token: String,
}

impl SimpleAuthenticatedClient {
    pub async fn from_token(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
        token: &str,
    ) -> Result<Self, OpenBaoError> {
        let rest_client = build_client(socket_file_path, Arc::clone(&logger)).await?;

        Ok(Self {
            rest_client: Box::new(rest_client),
            parser: JsonParser::new(),
            token: token.into(),
        })
    }

    pub async fn app_role_login(
        socket_file_path: PathBuf,
        logger: Arc<dyn Logger>,
        name: &str,
        secret_id: &str,
    ) -> Result<Self, OpenBaoError> {
        logger.info(&format!("Logging into OpenBao with app role '{name}'…"));

        let mut rest_client = build_client(socket_file_path, Arc::clone(&logger)).await?;
        let parser = JsonParser::new();

        let token = commands::auth::Login::new(
            &mut rest_client,
            &parser,
            &AuthType::AppRole,
            name,
            secret_id,
        )
        .perform()
        .await?;

        logger.info("Authenticated!");

        Ok(Self {
            rest_client: Box::new(rest_client),
            parser,
            token,
        })
    }
}

#[async_trait]
impl AuthenticatedClient for SimpleAuthenticatedClient {
    async fn upsert_acl_policy(
        &mut self,
        name: &str,
        policies: &[policy::Policy],
    ) -> Result<(), OpenBaoError> {
        UpsertAclPolicy::new(self.rest_client.as_mut(), &self.token, name, policies)
            .perform()
            .await
    }

    async fn has_auth(&mut self, auth_type: AuthType) -> Result<bool, OpenBaoError> {
        commands::auth::IsInstalledCommand::new(self.rest_client.as_mut(), &self.token, &auth_type)
            .perform()
            .await
    }

    async fn install_auth(
        &mut self,
        auth_type: AuthType,
        description: &str,
    ) -> Result<(), OpenBaoError> {
        commands::auth::InstallAuthCommand::new(
            self.rest_client.as_mut(),
            &self.token,
            &auth_type,
            description,
        )
        .perform()
        .await
    }

    async fn create_app_role(
        &mut self,
        auth_type: AuthType,
        name: &str,
        policies: Vec<&str>,
    ) -> Result<RoleId, OpenBaoError> {
        if !commands::auth::RoleExists::new(
            self.rest_client.as_mut(),
            &self.token,
            &auth_type,
            name,
        )
        .perform()
        .await?
        {
            commands::auth::CreateRole::new(
                self.rest_client.as_mut(),
                &self.token,
                &auth_type,
                name,
                policies,
            )
            .perform()
            .await?;
        }

        commands::auth::GetRoleId::new(
            self.rest_client.as_mut(),
            &self.parser,
            &self.token,
            &auth_type,
            name,
        )
        .perform()
        .await
    }

    async fn create_app_role_secret(
        &mut self,
        auth_type: AuthType,
        name: &str,
    ) -> Result<String, OpenBaoError> {
        if commands::auth::RoleExists::new(self.rest_client.as_mut(), &self.token, &auth_type, name)
            .perform()
            .await?
        {
            return commands::auth::CreateSecret::new(
                self.rest_client.as_mut(),
                &self.parser,
                &self.token,
                &auth_type,
                name,
            )
            .perform()
            .await;
        }

        Err(OpenBaoError::UnknownRole(name.into()))
    }

    async fn revoke_token(&mut self, token: &str) -> Result<(), OpenBaoError> {
        commands::auth::RevokeToken::new(self.rest_client.as_mut(), token)
            .perform()
            .await
    }
}
