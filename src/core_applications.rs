use docker::{Capability, ImageName};

use crate::application_definition::{ApplicationDefinition, MountFile};

pub(crate) fn open_bao() -> ApplicationDefinition {
    let config_file = MountFile::in_root(
        "config.hcl",
        r#"ui = false
    storage "file" {
        path = "/openbao/data"
    }

    listener "unix" {
        address = "/openbao/sockets/openbao.sock"
        socket_mode = "0660"
        socket_user = "${runas_user_id}"
        socket_group = "${douglas_group_id}"
    }

    api_addr = "http://openbao:8200"
    cluster_addr = "http://127.0.0.1:8201"

    log_level = "info"
    "#,
    );

    ApplicationDefinition::new(
        "openbao",
        ImageName::specific("openbao", "openbao", "2.4.3"),
    )
    .with_empty_mount("data", "/openbao/data")
    .with_empty_mount("log", "/openbao/log")
    .with_empty_shared_mount("sockets", "/openbao/sockets")
    .with_mount("config", "/openbao/config", vec![config_file])
    .with_environment_variable("VAULT_ADDR", "http://127.0.0.1:8200")
    .with_environment_variable("VAULT_API_ADDR", "http://openbao:8200")
    .with_command("server -config=/openbao/config/config.hcl")
    .with_label("traefik.enable", "true")
    .with_label(
        "traefik.http.routers.openbao.rule",
        "Host(`vault.localhost`)",
    )
    .with_label(
        "traefik.http.services.openbao.loadbalancer.server.port",
        "8200",
    )
    .with_capability(Capability::IpcLock)
    .with_capability(Capability::Chown)
}
