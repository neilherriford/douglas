use crate::bootstrap::core_seedlings;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
};
use config::DouglasFolders;
use file_system::{FileDeleter, FileReader, FileWriter, Inspect, Permissions};
use identity::Identity;
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use openbao_types::{Capability, Secret};
use seedbank::Name;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::time;

const SECRET_SHARES: u8 = 5;
const SECRET_THRESHOLD: u8 = 3;
const UNSEAL_CODES_FILE_NAME: &str = "unseal.codes";

const ACME_PKI_ROLE: &str = "traefik";
const ACME_PKI_DOMAINS: &str = "localhost";

const DOUGLAS_APP_ROLE_NAME: &str = "douglas.cli";
const DOUGLAS_ADMIN_POLICY_NAME: &str = "douglas-admin";

fn base_url() -> String {
    format!(
        "http://doug.{}:{}",
        core_seedlings::definitions::OPENBAO_NAME,
        core_seedlings::definitions::OPENBAO_TCP_PORT
    )
}

#[derive(Error, Debug)]
pub enum OpenBaoError {
    #[error("OpenBao error: {0}")]
    OpenBao(#[from] openbao::Error),
    #[error("File system error: {0}")]
    FileSystem(#[from] file_system::FileSystemError),
    #[error("Identity error: {0}")]
    Identity(#[from] identity::Error),
    #[error("Secrets are required for this operation")]
    SecretsRequired,
    #[error("OpenBao not running")]
    NotRunning,
    #[error("Unseal codes are required for this operation")]
    NoUnsealCodes,
    #[error("Douglas secrets failed")]
    DouglasSecretsFailed,
    #[error("Bract error: {0}")]
    Bract(#[from] bract_client::Error),
    #[error("Name parse error")]
    NameParse(#[from] seedbank::NameParseError),
    #[error("Base64 decode error")]
    Base64DecodeError(#[from] base64::DecodeError),
    #[error("AppRole credential error: {0}")]
    AppRole(#[from] openbao::app_role::AppRoleError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct Context<'a> {
    openbao_client: &'a mut dyn openbao::Client,
    file_reader: &'a dyn FileReader,
    file_writer: &'a dyn FileWriter,
    file_deleter: &'a dyn FileDeleter,
    permissions: &'a dyn Permissions,
    douglas_folders: &'a DouglasFolders,
    identity: &'a mut dyn Identity,
    unseal_codes: Option<Vec<Secret>>,
    root_token: Option<String>,
    douglas_role_id: Option<String>,
    douglas_secret_id: Option<String>,
}

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct State {
    is_running: bool,
    has_unseal_codes: bool,
    socket_exists: bool,
    douglas_credentials_available: bool,
    douglas_credentials_work: bool,
    is_initialized: bool,
    is_sealed: bool,
    kv_installed: bool,
    pki_installed: bool,
    ca_configured_installed: bool,
    acme_installed: bool,
    acme_pki_role_created: bool,
    app_role_installed: bool,
}

struct StateObserver<'a> {
    inspect: &'a dyn Inspect,
    file_reader: &'a dyn FileReader,
    identity: &'a mut dyn Identity,
    douglas_folders: &'a DouglasFolders,
    openbao_client_factory: &'a dyn openbao::ClientFactory,
    bract_client: &'a dyn bract_client::Client,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        inspect: &'a dyn Inspect,
        openbao_client_factory: &'a dyn openbao::ClientFactory,
        bract_client: &'a dyn bract_client::Client,
        file_reader: &'a dyn FileReader,
        identity: &'a mut dyn Identity,
        douglas_folders: &'a DouglasFolders,
    ) -> Self {
        Self {
            inspect,
            file_reader,
            identity,
            douglas_folders,
            openbao_client_factory,
            bract_client,
        }
    }

    pub async fn discover(&mut self, span: &Span) -> Result<State, OpenBaoError> {
        let guard = span
            .create_child("Checking OpenBao status", ScopeKind::Phase)
            .start_guard();

        let mut result = State::default();

        if !self.openbao_is_running().await? {
            return Ok(result);
        }
        result.is_running = true;

        result.douglas_credentials_available =
            openbao::app_role::available(self.file_reader, self.douglas_folders);
        result.has_unseal_codes = self.credential_file_exists(UNSEAL_CODES_FILE_NAME);

        let socket_path = get_socket_path(self.douglas_folders);
        result.socket_exists = self.eventually_exists(&socket_path).await;

        let mut openbao_client = self.openbao_client_factory.build(&socket_path).await?;
        let mut attempts = 0;
        loop {
            match openbao_client.status().await {
                Ok(status) => {
                    self.check_openbao_running_status(&mut result, &mut *openbao_client, status)
                        .await?;
                    break;
                }
                Err(err) if attempts == 5 => {
                    guard.finish_with_outcome(Outcome::Failed);
                    return Err(err.into());
                }
                Err(_) => {
                    attempts += 1;
                    time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        Ok(result)
    }

    async fn check_openbao_running_status(
        &mut self,
        result: &mut State,
        openbao_client: &mut dyn openbao::Client,
        status: openbao_types::Status,
    ) -> Result<(), OpenBaoError> {
        result.is_initialized = status.initialized;
        result.is_sealed = status.sealed;

        if status.sealed || !result.douglas_credentials_available {
            return Ok(());
        }

        let Ok(token) = openbao::app_role::login(
            openbao_client,
            self.file_reader,
            self.identity,
            self.douglas_folders,
        )
        .await
        else {
            return Ok(());
        };

        result.douglas_credentials_work = true;
        result.app_role_installed = Self::app_role_installed(openbao_client, &token).await;
        result.kv_installed =
            Self::is_mounted(openbao_client, &token, openbao_types::Mounts::KeyValueStore).await;
        result.pki_installed = Self::is_mounted(
            openbao_client,
            &token,
            openbao_types::Mounts::PublicKeyInfrastructure,
        )
        .await;
        result.acme_installed = Self::is_acme_enabled(openbao_client, &token).await;
        result.ca_configured_installed = Self::root_ca_is_configured(openbao_client, &token).await;
        result.acme_pki_role_created = Self::acme_pki_role_created(openbao_client, &token).await;

        Ok(())
    }

    fn credential_file_exists(&mut self, credential_file_name: &str) -> bool {
        let path = &self.douglas_folders.credential_file(credential_file_name);
        self.file_reader.exists(path)
    }

    async fn openbao_is_running(&self) -> Result<bool, OpenBaoError> {
        Ok(matches!(
            self.bract_client
                .seedling_status(&Name::from_str(core_seedlings::definitions::OPENBAO_NAME)?)
                .await?,
            bract::SeedlingStatus::Running(..)
        ))
    }

    async fn eventually_exists(&self, path: &Path) -> bool {
        let mut attempts = 0;

        while !self.inspect.exists(path) {
            attempts += 1;
            if attempts == 5 {
                return false;
            }
            time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    async fn app_role_installed(openbao_client: &mut dyn openbao::Client, token: &str) -> bool {
        let Ok(state) = openbao_client
            .is_auth_method_enabled(token, &openbao_types::AuthType::AppRole)
            .await
        else {
            return false;
        };
        state
    }

    async fn is_mounted(
        openbao_client: &mut dyn openbao::Client,
        token: &str,
        mount: openbao_types::Mounts,
    ) -> bool {
        let Ok(state) = openbao_client.is_mounted(token, mount).await else {
            return false;
        };
        state
    }

    async fn is_acme_enabled(openbao_client: &mut dyn openbao::Client, token: &str) -> bool {
        let Ok(state) = openbao_client.is_acme_enabled(token).await else {
            return false;
        };
        state
    }

    async fn root_ca_is_configured(openbao_client: &mut dyn openbao::Client, token: &str) -> bool {
        let Ok(state) = openbao_client.root_ca_is_configured(token).await else {
            return false;
        };
        state
    }

    async fn acme_pki_role_created(openbao_client: &mut dyn openbao::Client, token: &str) -> bool {
        let Ok(state) = openbao_client.pki_role_exists(token, ACME_PKI_ROLE).await else {
            return false;
        };
        state
    }
}

fn get_socket_path(douglas_folders: &DouglasFolders) -> PathBuf {
    let mut result = douglas_folders.seedling_mount(
        core_seedlings::definitions::OPENBAO_NAME,
        core_seedlings::definitions::OPENBAO_SOCKET_MOUNT_NAME,
    );
    result.push(core_seedlings::definitions::OPENBAO_SOCKET_NAME);
    result
}

fn get_unseal_codes_file_path(douglas_folders: &DouglasFolders) -> PathBuf {
    douglas_folders.credential_file(UNSEAL_CODES_FILE_NAME)
}

fn create_plan<'a>(state: &State) -> Result<Vec<Step<'a>>, OpenBaoError> {
    let mut result: Vec<Step<'a>> = vec![];

    if !state.socket_exists || !state.is_running {
        return Err(OpenBaoError::NotRunning);
    }

    if state.is_initialized {
        if state.is_sealed {
            if state.has_unseal_codes {
                push_step(&mut result, LoadUnsealCodes::default());
                push_step(&mut result, Unseal::default());
                push_step(&mut result, RecordSecrets::default());
                push_step(
                    &mut result,
                    Mount::new(openbao_types::Mounts::KeyValueStore),
                );
                push_step(
                    &mut result,
                    Mount::new(openbao_types::Mounts::PublicKeyInfrastructure),
                );
                push_step(&mut result, GenerateRootCA::default());
                push_step(&mut result, SetIssuingCRL::default());
                push_step(&mut result, ConfigureClusterPath::default());
                push_step(&mut result, EnableAcme::default());
                push_step(&mut result, CreateAcmePkiRole::default());
                push_step(&mut result, EnableAppRoleAuth::default());
                push_step(&mut result, CreateDouglasAppRolePolicy::default());
                push_step(&mut result, CreateDouglasAppRoleSecret::default());
                push_step(&mut result, StoreDouglasAppRoleCredentials::default());
                push_step(&mut result, RevokeAdminToken::default());
            } else {
                return Err(OpenBaoError::NoUnsealCodes);
            }
        } else {
            if !state.douglas_credentials_available {
                return Err(OpenBaoError::SecretsRequired);
            }
            if !state.douglas_credentials_work {
                return Err(OpenBaoError::DouglasSecretsFailed);
            }
            if !state.kv_installed {
                push_step(
                    &mut result,
                    Mount::new(openbao_types::Mounts::KeyValueStore),
                );
            }
            if !state.pki_installed {
                push_step(
                    &mut result,
                    Mount::new(openbao_types::Mounts::PublicKeyInfrastructure),
                );
            }
            if !state.ca_configured_installed {
                push_step(&mut result, GenerateRootCA::default());
            }
            push_step(&mut result, SetIssuingCRL::default());
            push_step(&mut result, ConfigureClusterPath::default());
            if !state.acme_installed {
                push_step(&mut result, EnableAcme::default());
            }
            if !state.acme_pki_role_created {
                push_step(&mut result, CreateAcmePkiRole::default());
            }
            if !state.app_role_installed {
                // hey claude yeah you -- do i need this lol thx
                push_step(&mut result, EnableAppRoleAuth::default());
            }
        }
    } else {
        push_step(&mut result, InitializeOpenBao::default());
        push_step(&mut result, Unseal::default());
        push_step(&mut result, RecordSecrets::default());
        push_step(
            &mut result,
            Mount::new(openbao_types::Mounts::KeyValueStore),
        );
        push_step(
            &mut result,
            Mount::new(openbao_types::Mounts::PublicKeyInfrastructure),
        );
        push_step(&mut result, GenerateRootCA::default());
        push_step(&mut result, SetIssuingCRL::default());
        push_step(&mut result, ConfigureClusterPath::default());
        push_step(&mut result, EnableAcme::default());
        push_step(&mut result, CreateAcmePkiRole::default());
        push_step(&mut result, EnableAppRoleAuth::default());
        push_step(&mut result, CreateDouglasAppRolePolicy::default());
        push_step(&mut result, CreateDouglasAppRoleSecret::default());
        push_step(&mut result, StoreDouglasAppRoleCredentials::default());
        push_step(&mut result, RevokeAdminToken::default());
    }

    Ok(result)
}

#[derive(Debug, Default)]
struct InitializeOpenBao {}

impl std::fmt::Display for InitializeOpenBao {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Initialize OpenBao")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for InitializeOpenBao {
    fn name(&self) -> String {
        "Initialize OpenBao".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Initializing OpenBao", ScopeKind::Step)
            .start_guard();

        let secrets = context
            .openbao_client
            .intialize(SECRET_SHARES, SECRET_THRESHOLD)
            .await?;

        context.unseal_codes = Some(secrets.secrets.clone());
        context.root_token = Some(secrets.root_token.clone());

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct LoadUnsealCodes {}

impl std::fmt::Display for LoadUnsealCodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Load unseal OpenBao")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for LoadUnsealCodes {
    fn name(&self) -> String {
        "Load unseal OpenBao".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Loading unseal OpenBao", ScopeKind::Step)
            .start_guard();

        let encrypted = context.file_reader.read_all(
            &context
                .douglas_folders
                .credential_file(UNSEAL_CODES_FILE_NAME),
        )?;

        let decrypted = context
            .identity
            .decrypt(&identity::Intent::Unseal, encrypted)?;

        let mut unseal_codes = Vec::new();

        for line in decrypted.split('\n') {
            unseal_codes.push(secret_from_base64(line)?);
        }

        context.unseal_codes = Some(unseal_codes);

        guard.finish(Ok(()))
    }
}

fn secret_from_base64(base64: &str) -> Result<openbao_types::Secret, OpenBaoError> {
    let bytes = STANDARD.decode(base64)?;
    let key = hex::encode(&bytes);
    Ok(openbao_types::Secret {
        key,
        base64: base64.to_string(),
    })
}

#[derive(Debug, Default)]
struct Unseal {}

impl std::fmt::Display for Unseal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unseal OpenBao")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for Unseal {
    fn name(&self) -> String {
        "Unseal OpenBao".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Unsealing OpenBao", ScopeKind::Step)
            .start_guard();

        let Some(unseal_codes) = &context.unseal_codes else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context.openbao_client.unseal(unseal_codes).await?;
        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct RecordSecrets {}

impl std::fmt::Display for RecordSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Record OpenBao secrets")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RecordSecrets {
    fn name(&self) -> String {
        "Record OpenBao secrets".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Record OpenBao secrets", ScopeKind::Step)
            .start_guard();

        let Some(unseal_codes) = &context.unseal_codes else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        let unseal_codes_file_path = get_unseal_codes_file_path(context.douglas_folders);
        if context.file_writer.exists(&unseal_codes_file_path) {
            guard
                .span()
                .message(Level::Info, "Secrets already exist, deleting…");
            context.file_deleter.delete(&unseal_codes_file_path)?;
        }

        let encrypted_unseal_codes = context.identity.encrypt(
            &identity::Intent::Unseal,
            unseal_codes
                .iter()
                .map(|secret| secret.base64.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )?;

        context
            .file_writer
            .write_all(&unseal_codes_file_path, &encrypted_unseal_codes)?;
        context.permissions.change_user_and_group_ownership(
            &unseal_codes_file_path,
            credentials::ROOT_USER_NAME,
            credentials::ROOT_GROUP_NAME,
        )?;
        context
            .permissions
            .change_mode(&unseal_codes_file_path, &file_system::Modes::OwnerReadWrite)?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug)]
struct Mount {
    mount: openbao_types::Mounts,
    description: String,
}

impl Mount {
    pub fn new(mount: openbao_types::Mounts) -> Self {
        Self {
            description: match &mount {
                openbao_types::Mounts::KeyValueStore => "key value store",
                openbao_types::Mounts::PublicKeyInfrastructure => "public key infrastructure",
            }
            .to_string(),
            mount,
        }
    }
}

impl std::fmt::Display for Mount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description)
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for Mount {
    fn name(&self) -> String {
        self.description.clone()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Mount key value store", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        if context
            .openbao_client
            .is_mounted(root_token, self.mount.clone())
            .await?
        {
            guard.span().message(Level::Info, "Already mounted!");
            return guard.finish(Ok(()));
        }

        context
            .openbao_client
            .mount(root_token, self.mount.clone())
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct EnableAppRoleAuth {}

impl std::fmt::Display for EnableAppRoleAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Enable AppRole auth method")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for EnableAppRoleAuth {
    fn name(&self) -> String {
        "Enable AppRole auth method".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Enabling AppRole auth method", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        if context
            .openbao_client
            .is_auth_method_enabled(root_token, &openbao_types::AuthType::AppRole)
            .await?
        {
            guard
                .span()
                .message(Level::Info, "AppRole auth method already enabled!");
            return guard.finish(Ok(()));
        }

        context
            .openbao_client
            .enable_auth_method(root_token, &openbao_types::AuthType::AppRole)
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct GenerateRootCA {}

impl std::fmt::Display for GenerateRootCA {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Generate root CA")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for GenerateRootCA {
    fn name(&self) -> String {
        "Generate root CA".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Generating root CA", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        if context
            .openbao_client
            .root_ca_is_configured(root_token)
            .await?
        {
            guard.span().message(Level::Info, "CA already configured!");
            return guard.finish(Ok(()));
        }

        context
            .openbao_client
            .generate_root_ca(root_token, "douglas")
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct SetIssuingCRL {}

impl std::fmt::Display for SetIssuingCRL {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Set issuing/CRL URLs")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for SetIssuingCRL {
    fn name(&self) -> String {
        "Set issuing/CRL URLs".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Setting issuing/CRL URLs", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context
            .openbao_client
            .set_issuing_crl(root_token, &base_url())
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct ConfigureClusterPath {}

impl std::fmt::Display for ConfigureClusterPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Configure cluster path")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ConfigureClusterPath {
    fn name(&self) -> String {
        "Configure cluster path".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Configuring cluster path", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context
            .openbao_client
            .set_cluster_url(root_token, &base_url())
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct EnableAcme {}

impl std::fmt::Display for EnableAcme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Enable ACME")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for EnableAcme {
    fn name(&self) -> String {
        "Enable ACME".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Enabling ACME", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context
            .openbao_client
            .set_acme_enabled(root_token, true)
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct CreateAcmePkiRole {}

impl std::fmt::Display for CreateAcmePkiRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create ACME PKI Role")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateAcmePkiRole {
    fn name(&self) -> String {
        "Create ACME PKI Role".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Creating ACME PKI Role", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context
            .openbao_client
            .create_pki_role(root_token, ACME_PKI_ROLE, ACME_PKI_DOMAINS)
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct CreateDouglasAppRolePolicy {}

impl std::fmt::Display for CreateDouglasAppRolePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create Douglas App Role Policy")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateDouglasAppRolePolicy {
    fn name(&self) -> String {
        "Create Douglas App Role Policy".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Creating Douglas App Role Policy", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        let policies = HashMap::from([
            ("sys/mounts".to_string(), HashSet::from([Capability::Read])),
            ("sys/auth".to_string(), HashSet::from([Capability::Read])),
            (
                "sys/mounts/kv".to_string(),
                HashSet::from([
                    Capability::Create,
                    Capability::Read,
                    Capability::Update,
                    Capability::Sudo,
                ]),
            ),
            (
                "sys/mounts/pki".to_string(),
                HashSet::from([
                    Capability::Create,
                    Capability::Read,
                    Capability::Update,
                    Capability::Sudo,
                ]),
            ),
            (
                "pki/*".to_string(),
                HashSet::from([Capability::Create, Capability::Read, Capability::Update]),
            ),
            (
                "sys/policies/acl/*".to_string(),
                HashSet::from([
                    Capability::Create,
                    Capability::Read,
                    Capability::Update,
                    Capability::Delete,
                ]),
            ),
            (
                "auth/approle/role/*".to_string(),
                HashSet::from([
                    Capability::Create,
                    Capability::Read,
                    Capability::Update,
                    Capability::Delete,
                    Capability::List,
                ]),
            ),
        ]);

        context
            .openbao_client
            .create_policy(root_token, DOUGLAS_ADMIN_POLICY_NAME, &policies)
            .await?;

        context
            .openbao_client
            .create_auth(
                root_token,
                &openbao_types::AuthType::AppRole,
                DOUGLAS_APP_ROLE_NAME,
                vec![DOUGLAS_ADMIN_POLICY_NAME.to_string()],
            )
            .await?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct CreateDouglasAppRoleSecret {}

impl std::fmt::Display for CreateDouglasAppRoleSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Create Douglas App Role Secret")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for CreateDouglasAppRoleSecret {
    fn name(&self) -> String {
        "Create Douglas App Role Secret".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Creating Douglas App Role Secret", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        let role_id = context
            .openbao_client
            .get_role_id(
                root_token,
                &openbao_types::AuthType::AppRole,
                DOUGLAS_APP_ROLE_NAME,
            )
            .await?;

        let secret_id = context
            .openbao_client
            .create_auth_secret(
                root_token,
                &openbao_types::AuthType::AppRole,
                DOUGLAS_APP_ROLE_NAME,
            )
            .await?;

        context.douglas_role_id = Some(role_id);
        context.douglas_secret_id = Some(secret_id);

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct StoreDouglasAppRoleCredentials {}

impl std::fmt::Display for StoreDouglasAppRoleCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Store Douglas App Role Secret")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StoreDouglasAppRoleCredentials {
    fn name(&self) -> String {
        "Store Douglas App Role Secret".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Storing Douglas App Role Secret", ScopeKind::Step)
            .start_guard();

        let Some(role_id) = context.douglas_role_id.clone() else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        let Some(secret_id) = context.douglas_secret_id.clone() else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        openbao::app_role::store(
            context.file_writer,
            context.file_deleter,
            context.permissions,
            context.identity,
            context.douglas_folders,
            role_id,
            secret_id,
        )?;

        guard.finish(Ok(()))
    }
}

#[derive(Debug, Default)]
struct RevokeAdminToken {}

impl std::fmt::Display for RevokeAdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Revoke root token")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RevokeAdminToken {
    fn name(&self) -> String {
        "Revoke root token".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Revoking root token", ScopeKind::Step)
            .start_guard();

        let Some(root_token) = &context.root_token else {
            guard.finish_with_outcome(Outcome::Failed);
            return Err(Box::new(OpenBaoError::SecretsRequired));
        };

        context.openbao_client.revoke_token(root_token).await?;

        guard.finish(Ok(()))
    }
}

pub async fn perform(
    reporter: Arc<dyn Reporter>,
    inspect: Arc<dyn Inspect>,
    openbao_client_factory: Arc<dyn openbao::ClientFactory>,
    bract_client: Arc<dyn bract_client::Client>,
    file_reader: Arc<dyn FileReader>,
    file_writer: Arc<dyn FileWriter>,
    file_deleter: Arc<dyn FileDeleter>,
    permissions: Arc<dyn Permissions>,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
) -> bool {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Bootstrapping OpenBao",
        log::ScopeKind::Group,
    )
    .start_guard();

    let state = {
        let mut state_observer = StateObserver::new(
            inspect.as_ref(),
            openbao_client_factory.as_ref(),
            bract_client.as_ref(),
            file_reader.as_ref(),
            identity,
            douglas_folders,
        );

        match state_observer.discover(guard.span()).await {
            Ok(state) => state,
            Err(err) => {
                guard.span().message(Level::Warn, &err.to_string());
                return false;
            }
        }
    };

    let socket_path = get_socket_path(douglas_folders);
    let mut openbao_client = match openbao_client_factory.build(&socket_path).await {
        Ok(client) => client,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
    };

    let plan = match resolve_plan::<Context, OpenBaoError>(guard.span(), create_plan(&state)) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
    };

    let mut context = Context {
        openbao_client: openbao_client.as_mut(),
        file_writer: file_writer.as_ref(),
        file_deleter: file_deleter.as_ref(),
        permissions: permissions.as_ref(),
        douglas_folders,
        identity,
        file_reader: file_reader.as_ref(),
        root_token: None,
        unseal_codes: None,
        douglas_role_id: None,
        douglas_secret_id: None,
    };

    if let Ok(()) = execute_plan(guard.span(), plan, &mut context, |_reason| ()).await {
        guard.finish_with_outcome(Outcome::Ok);
        true
    } else {
        guard.finish_with_outcome(Outcome::Failed);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_step_names(state: &State) -> Result<Vec<String>, OpenBaoError> {
        Ok(create_plan(state)?.iter().map(|step| step.name()).collect())
    }

    fn assert_plan_steps(state: &State, expected_step_names: &[&str]) {
        let Ok(step_names) = plan_step_names(state) else {
            panic!("should build a plan");
        };
        let step_names = step_names.iter().map(String::as_str).collect::<Vec<_>>();

        assert_eq!(step_names, expected_step_names);
    }

    #[test]
    fn create_plan_fails_when_not_running() {
        let state = State {
            is_running: false,
            socket_exists: true,
            ..State::default()
        };

        assert!(matches!(create_plan(&state), Err(OpenBaoError::NotRunning)));
    }

    #[test]
    fn create_plan_fails_when_socket_does_not_exist() {
        let state = State {
            is_running: true,
            socket_exists: false,
            ..State::default()
        };

        assert!(matches!(create_plan(&state), Err(OpenBaoError::NotRunning)));
    }

    #[test]
    fn create_plan_fails_when_sealed_without_unseal_codes() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: true,
            has_unseal_codes: false,
            ..State::default()
        };

        assert!(matches!(
            create_plan(&state),
            Err(OpenBaoError::NoUnsealCodes)
        ));
    }

    #[test]
    fn create_plan_runs_full_unseal_chain_when_sealed_with_unseal_codes() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: true,
            has_unseal_codes: true,
            ..State::default()
        };

        assert_plan_steps(
            &state,
            &[
                "Load unseal OpenBao",
                "Unseal OpenBao",
                "Record OpenBao secrets",
                "key value store",
                "public key infrastructure",
                "Generate root CA",
                "Set issuing/CRL URLs",
                "Configure cluster path",
                "Enable ACME",
                "Create ACME PKI Role",
                "Enable AppRole auth method",
                "Create Douglas App Role Policy",
                "Create Douglas App Role Secret",
                "Store Douglas App Role Secret",
                "Revoke root token",
            ],
        );
    }

    #[test]
    fn create_plan_runs_full_bootstrap_chain_when_not_initialized() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: false,
            ..State::default()
        };

        assert_plan_steps(
            &state,
            &[
                "Initialize OpenBao",
                "Unseal OpenBao",
                "Record OpenBao secrets",
                "key value store",
                "public key infrastructure",
                "Generate root CA",
                "Set issuing/CRL URLs",
                "Configure cluster path",
                "Enable ACME",
                "Create ACME PKI Role",
                "Enable AppRole auth method",
                "Create Douglas App Role Policy",
                "Create Douglas App Role Secret",
                "Store Douglas App Role Secret",
                "Revoke root token",
            ],
        );
    }

    #[test]
    fn create_plan_fails_when_unsealed_but_douglas_credential_files_are_missing() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: false,
            douglas_credentials_available: false,
            ..State::default()
        };

        assert!(matches!(
            create_plan(&state),
            Err(OpenBaoError::SecretsRequired)
        ));
    }

    #[test]
    fn create_plan_fails_when_unsealed_but_douglas_credentials_dont_work() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: false,
            douglas_credentials_available: true,
            douglas_credentials_work: false,
            ..State::default()
        };

        assert!(matches!(
            create_plan(&state),
            Err(OpenBaoError::DouglasSecretsFailed)
        ));
    }

    #[test]
    fn create_plan_only_reapplies_url_config_when_unsealed_and_everything_else_is_installed() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: false,
            has_unseal_codes: false,
            douglas_credentials_available: true,
            douglas_credentials_work: true,
            kv_installed: true,
            pki_installed: true,
            ca_configured_installed: true,
            acme_installed: true,
            acme_pki_role_created: true,
            app_role_installed: true,
        };

        assert_plan_steps(&state, &["Set issuing/CRL URLs", "Configure cluster path"]);
    }

    #[test]
    fn create_plan_tops_up_everything_when_unsealed_and_nothing_is_installed() {
        let state = State {
            is_running: true,
            socket_exists: true,
            is_initialized: true,
            is_sealed: false,
            has_unseal_codes: false,
            douglas_credentials_available: true,
            douglas_credentials_work: true,
            kv_installed: false,
            pki_installed: false,
            ca_configured_installed: false,
            acme_installed: false,
            acme_pki_role_created: false,
            app_role_installed: false,
        };

        assert_plan_steps(
            &state,
            &[
                "key value store",
                "public key infrastructure",
                "Generate root CA",
                "Set issuing/CRL URLs",
                "Configure cluster path",
                "Enable ACME",
                "Create ACME PKI Role",
                "Enable AppRole auth method",
            ],
        );
    }

    #[test]
    fn secret_from_base64_derives_the_matching_hex_key() {
        let Ok(secret) = secret_from_base64("AAECAw==") else {
            panic!("should decode base64");
        };

        assert_eq!(secret.base64, "AAECAw==");
        assert_eq!(secret.key, "00010203");
    }

    #[test]
    fn secret_from_base64_fails_on_invalid_base64() {
        assert!(secret_from_base64("not valid base64 !!!").is_err());
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockPermissions};
    use identity::MockIdentity;
    use log::Event;
    use openbao::MockClient as MockOpenBaoClient;
    use openbao_types::{AuthType, Mounts, Secrets};

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn test_span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Task)
    }

    async fn run_command<'a, C: Command<Context<'a>>>(mut command: C, context: &mut Context<'a>) {
        let Ok(()) = command.run(&test_span(), context).await else {
            panic!("command should succeed");
        };
    }

    struct Fixture {
        openbao_client: MockOpenBaoClient,
        file_reader: MockFileReader,
        file_writer: MockFileWriter,
        file_deleter: MockFileDeleter,
        permissions: MockPermissions,
        identity: MockIdentity,
        douglas_folders: DouglasFolders,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                openbao_client: MockOpenBaoClient::new(),
                file_reader: MockFileReader::new(),
                file_writer: MockFileWriter::new(),
                file_deleter: MockFileDeleter::new(),
                permissions: MockPermissions::new(),
                identity: MockIdentity::new(),
                douglas_folders: DouglasFolders::new(),
            }
        }

        fn context(&mut self) -> Context<'_> {
            Context {
                openbao_client: &mut self.openbao_client,
                file_reader: &self.file_reader,
                file_writer: &self.file_writer,
                file_deleter: &self.file_deleter,
                permissions: &self.permissions,
                douglas_folders: &self.douglas_folders,
                identity: &mut self.identity,
                unseal_codes: None,
                root_token: None,
                douglas_role_id: None,
                douglas_secret_id: None,
            }
        }
    }

    mod initialize_open_bao {
        use super::*;

        #[tokio::test]
        async fn run_should_record_the_unseal_codes_and_root_token() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_intialize()
                .withf(|shares, threshold| {
                    *shares == SECRET_SHARES && *threshold == SECRET_THRESHOLD
                })
                .returning(|_, _| {
                    Ok(Secrets {
                        secrets: vec![Secret {
                            key: "aa".to_string(),
                            base64: "AA==".to_string(),
                        }],
                        root_token: "root-token".to_string(),
                    })
                });

            let mut context = fixture.context();
            run_command(InitializeOpenBao::default(), &mut context).await;

            assert_eq!(context.root_token, Some("root-token".to_string()));
            assert_eq!(
                context.unseal_codes,
                Some(vec![Secret {
                    key: "aa".to_string(),
                    base64: "AA==".to_string()
                }])
            );
        }
    }

    mod load_unseal_codes {
        use super::*;

        #[tokio::test]
        async fn run_should_decrypt_and_parse_the_persisted_unseal_codes() {
            let mut fixture = Fixture::new();
            fixture
                .file_reader
                .expect_read_all()
                .returning(|_| Ok("encrypted-blob".to_string()));
            fixture
                .identity
                .expect_decrypt()
                .withf(|intent, cipher_text| {
                    matches!(intent, identity::Intent::Unseal) && cipher_text == "encrypted-blob"
                })
                .returning(|_, _| Ok("AAECAw==\nBAUGBw==".to_string()));

            let mut context = fixture.context();
            run_command(LoadUnsealCodes::default(), &mut context).await;

            assert_eq!(
                context.unseal_codes,
                Some(vec![
                    Secret {
                        key: "00010203".to_string(),
                        base64: "AAECAw==".to_string()
                    },
                    Secret {
                        key: "04050607".to_string(),
                        base64: "BAUGBw==".to_string()
                    },
                ])
            );
        }
    }

    mod unseal {
        use super::*;

        #[tokio::test]
        async fn run_should_fail_without_unseal_codes() {
            let mut fixture = Fixture::new();
            let mut context = fixture.context();

            let result = Unseal::default().run(&test_span(), &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn run_should_unseal_with_the_loaded_codes() {
            let mut fixture = Fixture::new();
            let codes = vec![Secret {
                key: "aa".to_string(),
                base64: "AA==".to_string(),
            }];
            fixture
                .openbao_client
                .expect_unseal()
                .withf({
                    let codes = codes.clone();
                    move |secrets| *secrets == codes
                })
                .returning(|_| Ok(()));

            let mut context = fixture.context();
            context.unseal_codes = Some(codes);

            run_command(Unseal::default(), &mut context).await;
        }
    }

    mod record_secrets {
        use super::*;

        #[tokio::test]
        async fn run_should_encrypt_and_write_the_unseal_codes() {
            let mut fixture = Fixture::new();
            fixture.file_writer.expect_exists().returning(|_| false);
            fixture
                .identity
                .expect_encrypt()
                .withf(|intent, plain_text| {
                    matches!(intent, identity::Intent::Unseal) && plain_text == "AA==\nBB=="
                })
                .returning(|_, _| Ok("encrypted".to_string()));
            fixture
                .file_writer
                .expect_write_all()
                .withf(|_, contents| contents == "encrypted")
                .returning(|_, _| Ok(()));
            fixture
                .permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            fixture
                .permissions
                .expect_change_mode()
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.unseal_codes = Some(vec![
                Secret {
                    key: "aa".to_string(),
                    base64: "AA==".to_string(),
                },
                Secret {
                    key: "bb".to_string(),
                    base64: "BB==".to_string(),
                },
            ]);

            run_command(RecordSecrets::default(), &mut context).await;
        }

        #[tokio::test]
        async fn run_should_delete_a_previously_recorded_file_first() {
            let mut fixture = Fixture::new();
            fixture.file_writer.expect_exists().returning(|_| true);
            fixture
                .file_deleter
                .expect_delete()
                .times(1)
                .returning(|_| Ok(()));
            fixture
                .identity
                .expect_encrypt()
                .returning(|_, _| Ok("encrypted".to_string()));
            fixture
                .file_writer
                .expect_write_all()
                .returning(|_, _| Ok(()));
            fixture
                .permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            fixture
                .permissions
                .expect_change_mode()
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.unseal_codes = Some(vec![Secret {
                key: "aa".to_string(),
                base64: "AA==".to_string(),
            }]);

            run_command(RecordSecrets::default(), &mut context).await;
        }
    }

    mod mount {
        use super::*;

        #[tokio::test]
        async fn run_should_fail_without_a_root_token() {
            let mut fixture = Fixture::new();
            let mut context = fixture.context();

            let result = Mount::new(Mounts::KeyValueStore)
                .run(&test_span(), &mut context)
                .await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn run_should_mount_when_not_already_mounted() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_is_mounted()
                .withf(|token, mount| token == "root-token" && *mount == Mounts::KeyValueStore)
                .returning(|_, _| Ok(false));
            fixture
                .openbao_client
                .expect_mount()
                .withf(|token, mount| token == "root-token" && *mount == Mounts::KeyValueStore)
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(Mount::new(Mounts::KeyValueStore), &mut context).await;
        }

        #[tokio::test]
        async fn run_should_skip_mounting_when_already_mounted() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_is_mounted()
                .returning(|_, _| Ok(true));
            fixture.openbao_client.expect_mount().times(0);

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(Mount::new(Mounts::PublicKeyInfrastructure), &mut context).await;
        }
    }

    mod generate_root_ca {
        use super::*;

        #[tokio::test]
        async fn run_should_generate_when_not_already_configured() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_root_ca_is_configured()
                .returning(|_| Ok(false));
            fixture
                .openbao_client
                .expect_generate_root_ca()
                .withf(|token, common_name| token == "root-token" && common_name == "douglas")
                .returning(|_, _| Ok("-----BEGIN CERTIFICATE-----".to_string()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(GenerateRootCA::default(), &mut context).await;
        }

        #[tokio::test]
        async fn run_should_skip_generating_when_already_configured() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_root_ca_is_configured()
                .returning(|_| Ok(true));
            fixture.openbao_client.expect_generate_root_ca().times(0);

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(GenerateRootCA::default(), &mut context).await;
        }
    }

    mod set_issuing_crl {
        use super::*;

        #[tokio::test]
        async fn run_should_set_the_issuing_and_crl_urls() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_set_issuing_crl()
                .withf(|token, url| token == "root-token" && url == base_url())
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(SetIssuingCRL::default(), &mut context).await;
        }
    }

    mod configure_cluster_path {
        use super::*;

        #[tokio::test]
        async fn run_should_set_the_cluster_url() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_set_cluster_url()
                .withf(|token, url| token == "root-token" && url == base_url())
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(ConfigureClusterPath::default(), &mut context).await;
        }
    }

    mod enable_acme {
        use super::*;

        #[tokio::test]
        async fn run_should_enable_acme() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_set_acme_enabled()
                .withf(|token, enabled| token == "root-token" && *enabled)
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(EnableAcme::default(), &mut context).await;
        }
    }

    mod create_acme_pki_role {
        use super::*;

        #[tokio::test]
        async fn run_should_create_the_configured_pki_role() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_create_pki_role()
                .withf(|token, role_name, allowed_domains| {
                    token == "root-token"
                        && role_name == ACME_PKI_ROLE
                        && allowed_domains == ACME_PKI_DOMAINS
                })
                .returning(|_, _, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(CreateAcmePkiRole::default(), &mut context).await;
        }
    }

    mod enable_app_role_auth {
        use super::*;

        #[tokio::test]
        async fn run_should_enable_when_not_already_enabled() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_is_auth_method_enabled()
                .withf(|token, auth_type| token == "root-token" && *auth_type == AuthType::AppRole)
                .returning(|_, _| Ok(false));
            fixture
                .openbao_client
                .expect_enable_auth_method()
                .withf(|token, auth_type| token == "root-token" && *auth_type == AuthType::AppRole)
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(EnableAppRoleAuth::default(), &mut context).await;
        }

        #[tokio::test]
        async fn run_should_skip_enabling_when_already_enabled() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_is_auth_method_enabled()
                .returning(|_, _| Ok(true));
            fixture.openbao_client.expect_enable_auth_method().times(0);

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(EnableAppRoleAuth::default(), &mut context).await;
        }
    }

    mod create_douglas_app_role_policy {
        use super::*;

        #[tokio::test]
        async fn run_should_create_the_policy_then_the_role_referencing_it() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_create_policy()
                .withf(|token, name, _policies| {
                    token == "root-token" && name == DOUGLAS_ADMIN_POLICY_NAME
                })
                .returning(|_, _, _| Ok(()));
            fixture
                .openbao_client
                .expect_create_auth()
                .withf(|token, auth_type, name, policy_names| {
                    token == "root-token"
                        && *auth_type == AuthType::AppRole
                        && name == DOUGLAS_APP_ROLE_NAME
                        && policy_names == &vec![DOUGLAS_ADMIN_POLICY_NAME.to_string()]
                })
                .returning(|_, _, _, _| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(CreateDouglasAppRolePolicy::default(), &mut context).await;
        }
    }

    mod create_douglas_app_role_secret {
        use super::*;

        #[tokio::test]
        async fn run_should_fetch_the_role_id_and_mint_a_secret_id() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_get_role_id()
                .returning(|_, _, _| Ok("role-1".to_string()));
            fixture
                .openbao_client
                .expect_create_auth_secret()
                .returning(|_, _, _| Ok("secret-1".to_string()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(CreateDouglasAppRoleSecret::default(), &mut context).await;

            assert_eq!(context.douglas_role_id, Some("role-1".to_string()));
            assert_eq!(context.douglas_secret_id, Some("secret-1".to_string()));
        }
    }

    mod store_douglas_app_role_credentials {
        use super::*;

        #[tokio::test]
        async fn run_should_fail_without_role_and_secret_ids() {
            let mut fixture = Fixture::new();
            let mut context = fixture.context();

            let result = StoreDouglasAppRoleCredentials::default()
                .run(&test_span(), &mut context)
                .await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn run_should_encrypt_and_write_both_credential_files() {
            let mut fixture = Fixture::new();
            fixture.file_writer.expect_exists().returning(|_| false);
            fixture
                .identity
                .expect_encrypt()
                .withf(|intent, _plain_text| matches!(intent, identity::Intent::Authentication))
                .returning(|_, plain_text| Ok(format!("encrypted:{plain_text}")));
            fixture
                .file_writer
                .expect_write_all()
                .returning(|_, _| Ok(()));
            fixture
                .permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            fixture
                .permissions
                .expect_change_mode()
                .returning(|_, _| Ok(()));

            let mut context = fixture.context();
            context.douglas_role_id = Some("role-1".to_string());
            context.douglas_secret_id = Some("secret-1".to_string());

            run_command(StoreDouglasAppRoleCredentials::default(), &mut context).await;
        }
    }

    mod revoke_admin_token {
        use super::*;

        #[tokio::test]
        async fn run_should_revoke_the_root_token() {
            let mut fixture = Fixture::new();
            fixture
                .openbao_client
                .expect_revoke_token()
                .withf(|token| token == "root-token")
                .returning(|_| Ok(()));

            let mut context = fixture.context();
            context.root_token = Some("root-token".to_string());

            run_command(RevokeAdminToken::default(), &mut context).await;
        }
    }
}
