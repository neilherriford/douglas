use crate::application_definition::{ApplicationDefinition, MountFile, MountTemplate};
use docker::{Capability, EnvironmentVariable, ImageName};
use openbao::OpenBaoError;
use std::path::PathBuf;

pub struct OpenBao {}

impl OpenBao {
    pub fn new() -> Self {
        Self {}
    }

    fn sockets_mount_name() -> String {
        "sockets".to_string()
    }

    fn socket_file_name() -> String {
        "openbao.sock".to_string()
    }

    pub async fn qualified_socket_path(
        &self,
        bract_client: bract::Client,
    ) -> Result<PathBuf, OpenBaoError> {
        let mut openbao_socket_path = match bract_client
            .active_mount_version(&self.name(), &OpenBao::sockets_mount_name())
            .await
        {
            Ok(None) => {
                return Err(OpenBaoError::Error(
                    "OpenBao not socket mount missing".into(),
                ));
            }
            Ok(Some(mount)) => mount.path,
            Err(err) => {
                return Err(OpenBaoError::Error(err.to_string()));
            }
        };

        openbao_socket_path.push(OpenBao::socket_file_name());
        Ok(openbao_socket_path)
    }

    fn data_mount() -> MountTemplate {
        MountTemplate::empty_mount("data", "/openbao/data")
    }

    fn log_mount() -> MountTemplate {
        MountTemplate::empty_mount("log", "/openbao/log")
    }

    fn sockets_mount() -> MountTemplate {
        MountTemplate::shared_empty_mount(&Self::sockets_mount_name(), "/openbao/sockets")
    }

    fn config_mount() -> MountTemplate {
        MountTemplate::populated_mount("config", "/openbao/config", vec![Self::config_file()])
    }

    fn config_file() -> MountFile {
        let contents = r#"ui = false
log_level = "info"

storage "file" {
    path = "/openbao/data"
}

listener "unix" {
    address = "/openbao/sockets/${socket_file_name}"
    socket_mode = "0660"
    socket_user = "${runas_user_id}"
    socket_group = "${douglas_group_id}"
}

listener "tcp" {
    address = "127.0.0.1:8201"
    tls_disable = true
}

api_addr = "unix:///openbao/sockets/${socket_file_name}"
cluster_addr = "http://127.0.0.1:8201"
"#;

        MountFile::in_root(
            "config.hcl",
            contents,
            vec![("socket_file_name", &Self::socket_file_name())],
        )
    }
}

impl ApplicationDefinition for OpenBao {
    fn name(&self) -> String {
        "openbao".to_string()
    }

    fn image_name(&self) -> ImageName {
        ImageName::specific("openbao", "openbao", "2.4.3")
    }

    fn command(&self) -> Option<String> {
        Some("server -config=/openbao/config/config.hcl".to_string())
    }

    fn environment_variables(&self) -> Vec<docker::EnvironmentVariable> {
        vec![
            EnvironmentVariable::new("VAULT_ADDR", "http://127.0.0.1:8200"),
            EnvironmentVariable::new("VAULT_API_ADDR", "http://openbao:8200"),
        ]
    }

    fn added_capabilities(&self) -> Vec<Capability> {
        vec![Capability::IpcLock, Capability::Chown]
    }

    fn mount_templates(&self) -> Vec<MountTemplate> {
        vec![
            Self::data_mount(),
            Self::log_mount(),
            Self::sockets_mount(),
            Self::config_mount(),
        ]
    }

    fn labels(&self) -> Vec<docker::Label> {
        vec![
            docker::Label::new("traefik.enable", "true"),
            docker::Label::new(
                "traefik.http.routers.openbao.rule",
                "Host(`vault.localhost`)",
            ),
            docker::Label::new(
                "traefik.http.services.openbao.loadbalancer.server.port",
                "8200",
            ),
        ]
    }
}
