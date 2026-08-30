use crate::blueprints::{container_name, openbao_socket_path};
use config::DouglasFolders;
use docker_types::Ipv4Subnet;
use file_system::{FileReader, FileSystemError};
use identity::Identity;
use openbao_types::{AuthType, Capability};
use seedbank_types::{Id, Name, NameParseError, SecretsAccess, SeedlingDefinition};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use thiserror::Error;

pub(crate) const AGENT_MOUNT_NAME: &str = "openbao-agent";
pub(crate) const AGENT_CONTAINER_MOUNT_PATH: &str = "/etc/openbao-agent";
const ROLE_ID_FILE_NAME: &str = "role_id";
const SECRET_ID_FILE_NAME: &str = "secret_id";
pub(crate) const AGENT_CONFIG_FILE_NAME: &str = "agent.json";
pub(crate) const SEEDLING_APPROLE_PREFIX: &str = "seedling.";

pub(crate) fn agent_account_full_name(seedling_name: &Name) -> String {
    format!("{seedling_name}-openbao-agent")
}

#[derive(Error, Debug)]
pub enum ProvisionSeedlingSecretsError {
    #[error("OpenBao error: {0}")]
    OpenBao(#[from] openbao::Error),
    #[error("AppRole login error: {0}")]
    AppRole(#[from] openbao::app_role::AppRoleError),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
    #[error("Name parse error: {0}")]
    NameParse(#[from] NameParseError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Seedling requested secrets but has no seedbank id to derive a private network from")]
    MissingSeedlingId,
}

pub struct SeedlingCredentials {
    pub role_id: String,
    pub secret_id: String,
}

fn approle_name(seedling_name: &Name) -> String {
    format!("{SEEDLING_APPROLE_PREFIX}{seedling_name}")
}

pub(crate) fn seedling_name_from_approle(role_name: &str) -> Option<Name> {
    role_name
        .strip_prefix(SEEDLING_APPROLE_PREFIX)
        .and_then(|name| name.parse().ok())
}

fn data_capabilities(access: SecretsAccess) -> HashSet<Capability> {
    let mut result = HashSet::new();
    if access.read {
        result.extend([Capability::Read, Capability::List]);
    }
    if access.write {
        result.extend([Capability::Create, Capability::Update, Capability::Delete]);
    }
    result
}

fn metadata_capabilities(access: SecretsAccess) -> HashSet<Capability> {
    let mut result = HashSet::new();
    if access.read {
        result.extend([Capability::Read, Capability::List]);
    }
    if access.write {
        result.insert(Capability::Delete);
    }
    result
}

fn policy_document(
    seedling_name: &Name,
    access: SecretsAccess,
) -> HashMap<String, HashSet<Capability>> {
    HashMap::from([
        (
            format!("kv/data/{seedling_name}/*"),
            data_capabilities(access),
        ),
        (
            format!("kv/metadata/{seedling_name}/*"),
            metadata_capabilities(access),
        ),
    ])
}

pub async fn execute(
    openbao_client: &mut dyn openbao::Client,
    admin_token: &str,
    seedling_name: &Name,
    access: SecretsAccess,
) -> Result<SeedlingCredentials, ProvisionSeedlingSecretsError> {
    let name = approle_name(seedling_name);

    openbao_client
        .create_policy(admin_token, &name, &policy_document(seedling_name, access))
        .await?;

    if !openbao_client
        .auth_exists(admin_token, &AuthType::AppRole, &name)
        .await?
    {
        openbao_client
            .create_auth(admin_token, &AuthType::AppRole, &name, vec![name.clone()])
            .await?;
    }

    for accessor in openbao_client
        .list_secret_id_accessors(admin_token, &AuthType::AppRole, &name)
        .await?
    {
        openbao_client
            .destroy_secret_id_accessor(admin_token, &AuthType::AppRole, &name, &accessor)
            .await?;
    }

    let role_id = openbao_client
        .get_role_id(admin_token, &AuthType::AppRole, &name)
        .await?;
    let secret_id = openbao_client
        .create_auth_secret(admin_token, &AuthType::AppRole, &name)
        .await?;

    Ok(SeedlingCredentials { role_id, secret_id })
}

pub async fn revoke(
    openbao_client: &mut dyn openbao::Client,
    admin_token: &str,
    seedling_name: &Name,
) -> Result<(), ProvisionSeedlingSecretsError> {
    let name = approle_name(seedling_name);

    if openbao_client
        .auth_exists(admin_token, &AuthType::AppRole, &name)
        .await?
    {
        openbao_client
            .delete_auth(admin_token, &AuthType::AppRole, &name)
            .await?;
    }

    openbao_client.delete_policy(admin_token, &name).await?;

    Ok(())
}

pub async fn revoke_if_provisioned(
    openbao_client_factory: &dyn openbao::ClientFactory,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
    seedling_name: &Name,
    definition: &SeedlingDefinition,
) -> Result<(), ProvisionSeedlingSecretsError> {
    if definition.secrets.is_none() {
        return Ok(());
    }

    let socket_path = openbao_socket_path(douglas_folders);
    let mut openbao_client = openbao_client_factory.build(&socket_path).await?;
    let admin_token = openbao::app_role::login(
        openbao_client.as_mut(),
        file_reader,
        identity,
        douglas_folders,
    )
    .await?;

    revoke(openbao_client.as_mut(), &admin_token, seedling_name).await
}

pub(crate) fn agent_mount_dir(douglas_folders: &DouglasFolders, seedling_name: &Name) -> PathBuf {
    douglas_folders.seedling_mount(seedling_name.as_ref(), AGENT_MOUNT_NAME)
}

fn openbao_container_address() -> String {
    let openbao_name: Name = openbao::SEEDLING_NAME
        .parse()
        .expect("openbao is a valid seedling name");
    let container = container_name(&openbao_name).expect("openbao is a valid container name");
    format!("http://{}:{}", container.as_ref(), openbao::API_PORT)
}

pub(crate) fn agent_private_network(id: &Id) -> (Ipv4Subnet, std::net::Ipv4Addr) {
    let high = (id.value >> 8) as u8;
    let low = (id.value & 0xFF) as u8;

    let subnet = Ipv4Subnet {
        cidr: format!("10.{high}.{low}.0/24"),
        gateway: std::net::Ipv4Addr::new(10, high, low, 1),
    };
    let agent_ip = std::net::Ipv4Addr::new(10, high, low, 2);

    (subnet, agent_ip)
}

fn render_agent_config(agent_ip: std::net::Ipv4Addr) -> Result<Vec<u8>, serde_json::Error> {
    let role_id_path = format!("{AGENT_CONTAINER_MOUNT_PATH}/{ROLE_ID_FILE_NAME}");
    let secret_id_path = format!("{AGENT_CONTAINER_MOUNT_PATH}/{SECRET_ID_FILE_NAME}");
    let listener_address = format!("{agent_ip}:{}", openbao::AGENT_LOCAL_PROXY_PORT);

    let config = serde_json::json!({
        "pid_file": "/tmp/openbao-agent.pid",
        "vault": {
            "address": openbao_container_address()
        },
        "auto_auth": {
            "method": [
                {
                    "type": "approle",
                    "config": {
                        "role_id_file_path": role_id_path,
                        "secret_id_file_path": secret_id_path,
                        "remove_secret_id_file_after_reading": true
                    }
                }
            ],
            "sink": [
                {
                    "type": "file",
                    "config": {
                        "path": "/tmp/openbao-agent-token"
                    }
                }
            ]
        },
        "api_proxy": {
            "use_auto_auth_token": true
        },
        "listener": [
            { "tcp": { "address": listener_address, "tls_disable": true } }
        ]
    });

    serde_json::to_vec_pretty(&config)
}

pub struct AgentProvisioning {
    pub mount_dir: PathBuf,
    pub role_id_path: PathBuf,
    pub secret_id_path: PathBuf,
    pub agent_config_path: PathBuf,
    pub role_id: String,
    pub secret_id: String,
    pub agent_config: Vec<u8>,
    pub private_subnet: Ipv4Subnet,
    pub agent_private_ip: std::net::Ipv4Addr,
}

pub async fn provision_agent_if_requested(
    openbao_client_factory: &dyn openbao::ClientFactory,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
    seedling_name: &Name,
    seedling_id: Option<&Id>,
    definition: &SeedlingDefinition,
) -> Result<Option<AgentProvisioning>, ProvisionSeedlingSecretsError> {
    let Some(access) = definition.secrets else {
        return Ok(None);
    };
    let seedling_id = seedling_id.ok_or(ProvisionSeedlingSecretsError::MissingSeedlingId)?;
    let (private_subnet, agent_private_ip) = agent_private_network(seedling_id);

    let mount_dir = agent_mount_dir(douglas_folders, seedling_name);
    let role_id_path = mount_dir.join(ROLE_ID_FILE_NAME);
    let secret_id_path = mount_dir.join(SECRET_ID_FILE_NAME);
    let agent_config_path = mount_dir.join(AGENT_CONFIG_FILE_NAME);

    let (role_id, secret_id) =
        if file_reader.exists(&role_id_path) && file_reader.exists(&secret_id_path) {
            (
                file_reader.read_all(&role_id_path)?,
                file_reader.read_all(&secret_id_path)?,
            )
        } else {
            let socket_path = openbao_socket_path(douglas_folders);
            let mut openbao_client = openbao_client_factory.build(&socket_path).await?;
            let admin_token = openbao::app_role::login(
                openbao_client.as_mut(),
                file_reader,
                identity,
                douglas_folders,
            )
            .await?;

            let credentials =
                execute(openbao_client.as_mut(), &admin_token, seedling_name, access).await?;
            (credentials.role_id, credentials.secret_id)
        };

    Ok(Some(AgentProvisioning {
        mount_dir,
        role_id_path,
        secret_id_path,
        agent_config_path,
        role_id,
        secret_id,
        agent_config: render_agent_config(agent_private_ip)?,
        private_subnet,
        agent_private_ip,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> Name {
        value.parse().expect("valid seedling name")
    }

    #[tokio::test]
    async fn execute_should_create_a_policy_scoped_to_the_seedlings_own_kv_namespace() {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_create_policy()
            .withf(|_, policy_name, policies| {
                policy_name == "seedling.hello-openbao"
                    && policies.get("kv/data/hello-openbao/*")
                        == Some(&HashSet::from([Capability::Read, Capability::List]))
                    && policies.get("kv/metadata/hello-openbao/*")
                        == Some(&HashSet::from([Capability::Read, Capability::List]))
            })
            .returning(|_, _, _| Ok(()));
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(true));
        openbao_client.expect_create_auth().times(0);
        openbao_client
            .expect_list_secret_id_accessors()
            .returning(|_, _, _| Ok(Vec::new()));
        openbao_client
            .expect_get_role_id()
            .returning(|_, _, _| Ok("role-id".to_string()));
        openbao_client
            .expect_create_auth_secret()
            .returning(|_, _, _| Ok("secret-id".to_string()));

        let credentials = execute(
            &mut openbao_client,
            "admin-token",
            &name("hello-openbao"),
            SecretsAccess {
                read: true,
                write: false,
            },
        )
        .await
        .expect("should provision credentials");

        assert_eq!(credentials.role_id, "role-id");
        assert_eq!(credentials.secret_id, "secret-id");
    }

    #[tokio::test]
    async fn execute_should_create_the_approle_only_when_it_does_not_already_exist() {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_create_policy()
            .returning(|_, _, _| Ok(()));
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(false));
        openbao_client
            .expect_create_auth()
            .withf(|_, auth_type, name, policy_names| {
                *auth_type == AuthType::AppRole
                    && name == "seedling.hello-openbao"
                    && policy_names == &vec!["seedling.hello-openbao".to_string()]
            })
            .returning(|_, _, _, _| Ok(()));
        openbao_client
            .expect_list_secret_id_accessors()
            .returning(|_, _, _| Ok(Vec::new()));
        openbao_client
            .expect_get_role_id()
            .returning(|_, _, _| Ok("role-id".to_string()));
        openbao_client
            .expect_create_auth_secret()
            .returning(|_, _, _| Ok("secret-id".to_string()));

        let result = execute(
            &mut openbao_client,
            "admin-token",
            &name("hello-openbao"),
            SecretsAccess {
                read: true,
                write: true,
            },
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn execute_should_destroy_any_previously_issued_secret_ids_before_minting_a_new_one() {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_create_policy()
            .returning(|_, _, _| Ok(()));
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(true));
        openbao_client
            .expect_list_secret_id_accessors()
            .returning(|_, _, _| Ok(vec!["stale-1".to_string(), "stale-2".to_string()]));
        openbao_client
            .expect_destroy_secret_id_accessor()
            .withf(|_, _, name, accessor| name == "seedling.hello-openbao" && accessor == "stale-1")
            .returning(|_, _, _, _| Ok(()));
        openbao_client
            .expect_destroy_secret_id_accessor()
            .withf(|_, _, name, accessor| name == "seedling.hello-openbao" && accessor == "stale-2")
            .returning(|_, _, _, _| Ok(()));
        openbao_client
            .expect_get_role_id()
            .returning(|_, _, _| Ok("role-id".to_string()));
        openbao_client
            .expect_create_auth_secret()
            .returning(|_, _, _| Ok("secret-id".to_string()));

        let credentials = execute(
            &mut openbao_client,
            "admin-token",
            &name("hello-openbao"),
            SecretsAccess {
                read: true,
                write: true,
            },
        )
        .await
        .expect("should provision credentials");

        assert_eq!(credentials.secret_id, "secret-id");
    }

    #[tokio::test]
    async fn revoke_should_delete_the_role_and_policy_when_the_role_exists() {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(true));
        openbao_client
            .expect_delete_auth()
            .withf(|_, auth_type, name| {
                *auth_type == AuthType::AppRole && name == "seedling.hello-openbao"
            })
            .returning(|_, _, _| Ok(()));
        openbao_client
            .expect_delete_policy()
            .withf(|_, name| name == "seedling.hello-openbao")
            .returning(|_, _| Ok(()));

        let result = revoke(&mut openbao_client, "admin-token", &name("hello-openbao")).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn revoke_should_skip_deleting_a_role_that_no_longer_exists() {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(false));
        openbao_client.expect_delete_auth().times(0);
        openbao_client
            .expect_delete_policy()
            .returning(|_, _| Ok(()));

        let result = revoke(&mut openbao_client, "admin-token", &name("hello-openbao")).await;

        assert!(result.is_ok());
    }

    fn folders() -> DouglasFolders {
        DouglasFolders {
            logs: PathBuf::from("/var/log/douglas/"),
            transients: PathBuf::from("/run/douglas/"),
            configs: PathBuf::from("/etc/douglas/"),
            seedlings_root: PathBuf::from("/var/lib/douglas/"),
            identity: PathBuf::from("/var/lib/douglas-identity/"),
        }
    }

    fn definition_with_secrets(access: Option<SecretsAccess>) -> SeedlingDefinition {
        SeedlingDefinition::new(
            docker_types::VersionedImageName::latest("test"),
            HashMap::new(),
            seedbank_types::Routing::None,
        )
        .with_secrets_access(access)
    }

    #[tokio::test]
    async fn provision_agent_should_return_none_when_secrets_are_not_requested() {
        let openbao_client_factory = openbao::MockClientFactory::new();
        let file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let definition = definition_with_secrets(None);

        let provisioning = provision_agent_if_requested(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &folders(),
            &name("hello-openbao"),
            None,
            &definition,
        )
        .await
        .expect("should not error");

        assert!(provisioning.is_none());
    }

    #[tokio::test]
    async fn provision_agent_should_reuse_previously_provisioned_credentials_from_disk() {
        let openbao_client_factory = openbao::MockClientFactory::new();
        let mut file_reader = file_system::MockFileReader::new();
        file_reader.expect_exists().returning(|_| true);
        file_reader
            .expect_read_all()
            .withf(|path| path.ends_with("role_id"))
            .returning(|_| Ok("existing-role-id".to_string()));
        file_reader
            .expect_read_all()
            .withf(|path| path.ends_with("secret_id"))
            .returning(|_| Ok("existing-secret-id".to_string()));
        let mut identity = identity::MockIdentity::new();

        let definition = definition_with_secrets(Some(SecretsAccess {
            read: true,
            write: true,
        }));

        let provisioning = provision_agent_if_requested(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &folders(),
            &name("hello-openbao"),
            Some(&Id { value: 7 }),
            &definition,
        )
        .await
        .expect("should reuse existing credentials")
        .expect("secrets were requested");

        assert_eq!(provisioning.role_id, "existing-role-id");
        assert_eq!(provisioning.secret_id, "existing-secret-id");
        assert!(provisioning.mount_dir.ends_with("openbao-agent"));
        assert_eq!(
            provisioning.agent_private_ip,
            std::net::Ipv4Addr::new(10, 0, 7, 2)
        );
        assert_eq!(provisioning.private_subnet.cidr, "10.0.7.0/24");
    }

    #[tokio::test]
    async fn provision_agent_should_error_when_secrets_are_requested_without_a_seedling_id() {
        let openbao_client_factory = openbao::MockClientFactory::new();
        let file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let definition = definition_with_secrets(Some(SecretsAccess {
            read: true,
            write: true,
        }));

        let result = provision_agent_if_requested(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &folders(),
            &name("hello-openbao"),
            None,
            &definition,
        )
        .await;

        assert!(matches!(
            result,
            Err(ProvisionSeedlingSecretsError::MissingSeedlingId)
        ));
    }

    #[test]
    fn agent_private_network_should_derive_a_distinct_subnet_and_fixed_agent_address_per_id() {
        let (subnet, agent_ip) = agent_private_network(&Id { value: 300 });

        assert_eq!(subnet.cidr, "10.1.44.0/24");
        assert_eq!(subnet.gateway, std::net::Ipv4Addr::new(10, 1, 44, 1));
        assert_eq!(agent_ip, std::net::Ipv4Addr::new(10, 1, 44, 2));
    }

    #[test]
    fn render_agent_config_should_reference_the_mounted_credential_files_and_local_proxy_port() {
        let bytes = render_agent_config(std::net::Ipv4Addr::new(10, 0, 7, 2))
            .expect("should render valid json");
        let config: serde_json::Value =
            serde_json::from_slice(&bytes).expect("should parse as json");

        assert_eq!(
            config["vault"]["address"],
            serde_json::json!("http://doug.openbao:8201")
        );

        let method = &config["auto_auth"]["method"][0];
        assert_eq!(method["type"], serde_json::json!("approle"));
        assert_eq!(
            method["config"]["role_id_file_path"],
            serde_json::json!("/etc/openbao-agent/role_id")
        );
        assert_eq!(
            method["config"]["secret_id_file_path"],
            serde_json::json!("/etc/openbao-agent/secret_id")
        );
        assert_eq!(
            method["config"]["remove_secret_id_file_after_reading"],
            serde_json::json!(true)
        );

        assert_eq!(
            config["auto_auth"]["sink"][0]["type"],
            serde_json::json!("file")
        );
        assert_eq!(
            config["api_proxy"]["use_auto_auth_token"],
            serde_json::json!(true)
        );
        assert_eq!(
            config["listener"][0]["tcp"]["address"],
            serde_json::json!("10.0.7.2:8100")
        );
        assert_eq!(
            config["listener"][0]["tcp"]["tls_disable"],
            serde_json::json!(true)
        );
    }

    #[tokio::test]
    async fn revoke_if_provisioned_should_do_nothing_when_secrets_were_never_requested() {
        let openbao_client_factory = openbao::MockClientFactory::new();
        let file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let definition = definition_with_secrets(None);

        let result = revoke_if_provisioned(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &folders(),
            &name("hello-openbao"),
            &definition,
        )
        .await;

        assert!(result.is_ok());
    }

    #[test]
    fn data_capabilities_should_be_empty_when_neither_read_nor_write_is_requested() {
        assert!(
            data_capabilities(SecretsAccess {
                read: false,
                write: false
            })
            .is_empty()
        );
    }

    #[test]
    fn metadata_capabilities_should_only_grant_delete_for_write_access() {
        let capabilities = metadata_capabilities(SecretsAccess {
            read: false,
            write: true,
        });

        assert_eq!(capabilities, HashSet::from([Capability::Delete]));
    }

    #[test]
    fn seedling_name_from_approle_should_strip_the_prefix() {
        assert_eq!(
            seedling_name_from_approle("seedling.hello-openbao"),
            Some(name("hello-openbao"))
        );
    }

    #[test]
    fn seedling_name_from_approle_should_be_none_for_an_unrelated_role() {
        assert_eq!(seedling_name_from_approle("douglas.cli"), None);
    }
}
