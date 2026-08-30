use crate::{
    Error,
    commands::{self},
};
use async_trait::async_trait;
use log::Reporter;
use openbao_types::{AuthType, Capability, Mounts, Secret, Secrets, Status};
use simple_rest_client::{RestClient, parsers::json::JsonParser, unix_domain_socket::build_client};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait Client: Send + Sync {
    async fn status(&mut self) -> Result<Status, Error>;
    async fn intialize(
        &mut self,
        secret_shares: u8,
        secret_threshold: u8,
    ) -> Result<Secrets, Error>;
    async fn is_mounted(&mut self, token: &str, mount: Mounts) -> Result<bool, Error>;
    async fn mount(&mut self, token: &str, mount: Mounts) -> Result<(), Error>;
    async fn list_mounts(&mut self, token: &str) -> Result<HashMap<String, String>, Error>;
    async fn is_auth_method_enabled(
        &mut self,
        token: &str,
        auth_type: &AuthType,
    ) -> Result<bool, Error>;
    async fn enable_auth_method(&mut self, token: &str, auth_type: &AuthType) -> Result<(), Error>;
    async fn login(
        &mut self,
        auth_type: &AuthType,
        role_id: &str,
        secret: &str,
    ) -> Result<String, Error>;
    async fn generate_root_ca(&mut self, token: &str, common_name: &str) -> Result<String, Error>;
    async fn root_ca_is_configured(&mut self, token: &str) -> Result<bool, Error>;
    async fn set_issuing_crl(&mut self, token: &str, base_url: &str) -> Result<(), Error>;
    async fn set_cluster_url(&mut self, token: &str, base_url: &str) -> Result<(), Error>;
    async fn set_acme_enabled(&mut self, token: &str, enabled: bool) -> Result<(), Error>;
    async fn is_acme_enabled(&mut self, token: &str) -> Result<bool, Error>;
    async fn create_pki_role(
        &mut self,
        token: &str,
        role_name: &str,
        allowed_domains: &str,
    ) -> Result<(), Error>;
    async fn pki_role_exists(&mut self, token: &str, role_name: &str) -> Result<bool, Error>;
    async fn auth_exists(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<bool, Error>;
    async fn list_auth_roles(
        &mut self,
        token: &str,
        auth_type: &AuthType,
    ) -> Result<Vec<String>, Error>;
    async fn create_policy(
        &mut self,
        token: &str,
        name: &str,
        policies: &HashMap<String, HashSet<Capability>>,
    ) -> Result<(), Error>;
    async fn create_auth(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
        policy_names: Vec<String>,
    ) -> Result<(), Error>;
    async fn delete_auth(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<(), Error>;
    async fn delete_policy(&mut self, token: &str, name: &str) -> Result<(), Error>;
    async fn list_policies(&mut self, token: &str) -> Result<Vec<String>, Error>;
    async fn get_role_id(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<String, Error>;
    async fn create_auth_secret(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<String, Error>;
    async fn list_secret_id_accessors(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<Vec<String>, Error>;
    async fn destroy_secret_id_accessor(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
        accessor: &str,
    ) -> Result<(), Error>;
    async fn revoke_token(&mut self, token: &str) -> Result<(), Error>;
    async fn unseal(&mut self, secrets: &[Secret]) -> Result<(), Error>;
}

pub struct SocketClient {
    reporter: Arc<dyn Reporter>,
    rest_client: Box<dyn RestClient>,
    parser: JsonParser,
}

impl SocketClient {
    pub async fn build(
        reporter: Arc<dyn Reporter>,
        socket_file_path: PathBuf,
    ) -> Result<Self, Error> {
        let rest_client = build_client(
            socket_file_path,
            simple_rest_client::ServerClosedConnections::TreatAsError, // TODO: is this true?
        )
        .await?;

        Ok(Self {
            reporter,
            rest_client: Box::new(rest_client),
            parser: JsonParser::new(),
        })
    }
}

#[async_trait]
impl Client for SocketClient {
    async fn status(&mut self) -> Result<Status, Error> {
        commands::status::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
        )
        .await
    }

    async fn intialize(
        &mut self,
        secret_shares: u8,
        secret_threshold: u8,
    ) -> Result<Secrets, Error> {
        commands::init::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            secret_shares,
            secret_threshold,
        )
        .await
    }

    async fn is_mounted(&mut self, token: &str, mount: Mounts) -> Result<bool, Error> {
        let result = commands::mounts::list(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await?;

        Ok(result.contains_key(&format!("{mount}/")))
    }

    async fn mount(&mut self, token: &str, mount: Mounts) -> Result<(), Error> {
        commands::mounts::create(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            mount,
        )
        .await
    }

    async fn list_mounts(&mut self, token: &str) -> Result<HashMap<String, String>, Error> {
        commands::mounts::list(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await
    }

    async fn is_auth_method_enabled(
        &mut self,
        token: &str,
        auth_type: &AuthType,
    ) -> Result<bool, Error> {
        let result = commands::auth::list_auth_methods(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await?;

        Ok(result.contains_key(&format!("{auth_type}/")))
    }

    async fn enable_auth_method(&mut self, token: &str, auth_type: &AuthType) -> Result<(), Error> {
        commands::auth::enable_auth_method(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            auth_type,
        )
        .await
    }

    async fn login(
        &mut self,
        auth_type: &AuthType,
        name: &str,
        secret: &str,
    ) -> Result<String, Error> {
        commands::log_in::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            auth_type,
            name,
            secret,
        )
        .await
    }

    async fn generate_root_ca(&mut self, token: &str, common_name: &str) -> Result<String, Error> {
        commands::root_ca::generate(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
            common_name,
        )
        .await
    }

    async fn root_ca_is_configured(&mut self, token: &str) -> Result<bool, Error> {
        commands::root_ca::is_configured(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await
    }

    async fn set_issuing_crl(&mut self, token: &str, base_url: &str) -> Result<(), Error> {
        commands::configure_urls::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            base_url,
        )
        .await
    }

    async fn set_cluster_url(&mut self, token: &str, base_url: &str) -> Result<(), Error> {
        commands::configure_cluster::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            base_url,
        )
        .await
    }

    async fn set_acme_enabled(&mut self, token: &str, enabled: bool) -> Result<(), Error> {
        commands::acme::enable(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            enabled,
        )
        .await
    }

    async fn is_acme_enabled(&mut self, token: &str) -> Result<bool, Error> {
        commands::acme::is_enabled(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await
    }

    async fn create_pki_role(
        &mut self,
        token: &str,
        role_name: &str,
        allowed_domains: &str,
    ) -> Result<(), Error> {
        commands::pki_role::create(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            role_name,
            allowed_domains,
        )
        .await
    }

    async fn pki_role_exists(&mut self, token: &str, role_name: &str) -> Result<bool, Error> {
        commands::pki_role::exists(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            role_name,
        )
        .await
    }

    async fn auth_exists(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<bool, Error> {
        commands::auth::exists(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            auth_type,
            name,
        )
        .await
    }

    async fn list_auth_roles(
        &mut self,
        token: &str,
        auth_type: &AuthType,
    ) -> Result<Vec<String>, Error> {
        commands::auth::list_roles(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
            auth_type,
        )
        .await
    }

    async fn create_policy(
        &mut self,
        token: &str,
        name: &str,
        policies: &HashMap<String, HashSet<Capability>>,
    ) -> Result<(), Error> {
        commands::acl_policy::upsert(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            name,
            policies,
        )
        .await
    }

    async fn create_auth(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
        policy_names: Vec<String>,
    ) -> Result<(), Error> {
        commands::auth::create(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            auth_type,
            name,
            policy_names,
        )
        .await
    }

    async fn delete_auth(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<(), Error> {
        commands::auth::delete_role(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            auth_type,
            name,
        )
        .await
    }

    async fn delete_policy(&mut self, token: &str, name: &str) -> Result<(), Error> {
        commands::acl_policy::delete(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            name,
        )
        .await
    }

    async fn list_policies(&mut self, token: &str) -> Result<Vec<String>, Error> {
        commands::acl_policy::list(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
        )
        .await
    }

    async fn get_role_id(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<String, Error> {
        commands::auth::get_role_id(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
            auth_type,
            name,
        )
        .await
    }

    async fn create_auth_secret(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<String, Error> {
        commands::auth::create_secret(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
            auth_type,
            name,
        )
        .await
    }

    async fn list_secret_id_accessors(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
    ) -> Result<Vec<String>, Error> {
        commands::auth::list_secret_id_accessors(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            token,
            auth_type,
            name,
        )
        .await
    }

    async fn destroy_secret_id_accessor(
        &mut self,
        token: &str,
        auth_type: &AuthType,
        name: &str,
        accessor: &str,
    ) -> Result<(), Error> {
        commands::auth::destroy_secret_id_accessor(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            token,
            auth_type,
            name,
            accessor,
        )
        .await
    }

    async fn revoke_token(&mut self, token: &str) -> Result<(), Error> {
        commands::auth::revoke(Arc::clone(&self.reporter), self.rest_client.as_mut(), token).await
    }

    async fn unseal(&mut self, secrets: &[Secret]) -> Result<(), Error> {
        commands::unseal::execute(
            Arc::clone(&self.reporter),
            self.rest_client.as_mut(),
            &self.parser,
            secrets,
        )
        .await
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait ClientFactory: Send + Sync {
    async fn build(&self, socket_path: &Path) -> Result<Box<dyn Client>, Error>;
}

pub struct SocketClientFactory {
    reporter: Arc<dyn Reporter>,
}

impl SocketClientFactory {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self { reporter }
    }
}

#[async_trait]
impl ClientFactory for SocketClientFactory {
    async fn build(&self, socket_path: &Path) -> Result<Box<dyn Client>, Error> {
        Ok(Box::new(
            SocketClient::build(Arc::clone(&self.reporter), socket_path.to_path_buf()).await?,
        ))
    }
}
