use config::DouglasFolders;
use file_system::Modes;
use std::path::PathBuf;

pub mod bootstrap;
pub(crate) mod drop_seedling;
pub(crate) mod new_seedling;
pub(crate) mod reconcile_seedling;
pub(crate) mod start_seedling;
pub(crate) mod stop_seedling;
pub(crate) mod write_traefik_routes;

const EXPECTED_MOUNT_MODE: Modes = Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute;
const CONTAINER_NAME_PREFIX: &str = "doug.";
pub(crate) const SYSTEM_NETWORK_NAME: &str = "douglas-system";
const TRAEFIK_SEEDLING_NAME: &str = "traefik";
const TRAEFIK_CONFIG_MOUNT_NAME: &str = "config";
const TRAEFIK_DYNAMIC_DIR_NAME: &str = "dynamic";

pub(crate) fn traefik_dynamic_dir(
    douglas_folders: &DouglasFolders,
) -> Result<PathBuf, seedbank_types::NameParseError> {
    let traefik_name: seedbank_types::Name = TRAEFIK_SEEDLING_NAME.parse()?;
    let config_mount_name: seedbank_types::Name = TRAEFIK_CONFIG_MOUNT_NAME.parse()?;

    let mut dynamic_dir = seedling_mount_path(douglas_folders, &traefik_name, &config_mount_name);
    dynamic_dir.push(TRAEFIK_DYNAMIC_DIR_NAME);
    Ok(dynamic_dir)
}

pub(crate) fn seedling_network_name(
    seedling_name: &seedbank_types::Name,
) -> Result<docker_types::NetworkName, docker_types::DockerNameError> {
    container_name(seedling_name)?.as_ref().parse()
}

pub(crate) fn container_name(
    seedling_name: &seedbank_types::Name,
) -> Result<docker_types::ContainerName, docker_types::DockerNameError> {
    format!("{CONTAINER_NAME_PREFIX}{}", seedling_name.as_ref()).parse()
}

fn seedling_mount_path(
    douglas_folders: &DouglasFolders,
    seedling_name: &seedbank_types::Name,
    mount_name: &seedbank_types::Name,
) -> PathBuf {
    let mut result = douglas_folders.application_mounts.clone();
    result.push(seedling_name.as_ref());
    result.push(mount_name.as_ref());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_name_should_add_the_douglas_prefix() {
        let seedling_name: seedbank_types::Name = "traefik".parse().unwrap();

        let result = container_name(&seedling_name).expect("should be a valid container name");

        assert_eq!(result.as_ref(), "doug.traefik");
    }

    #[test]
    fn test_seedling_network_name_should_reuse_the_container_name() {
        let seedling_name: seedbank_types::Name = "hello-world".parse().unwrap();

        let result = seedling_network_name(&seedling_name).expect("should be a valid network name");

        assert_eq!(result.as_ref(), "doug.hello-world");
    }

    #[test]
    fn test_traefik_dynamic_dir_should_nest_under_traefiks_config_mount() {
        let douglas_folders = DouglasFolders::new();

        let result =
            traefik_dynamic_dir(&douglas_folders).expect("should build a valid dynamic dir path");

        let mut expected = douglas_folders.application_mounts.clone();
        expected.push("traefik");
        expected.push("config");
        expected.push("dynamic");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_seedling_mount_path_should_nest_the_mount_under_the_seedling() {
        let douglas_folders = DouglasFolders::new();
        let seedling_name: seedbank_types::Name = "traefik".parse().unwrap();
        let mount_name: seedbank_types::Name = "shared".parse().unwrap();

        let result = seedling_mount_path(&douglas_folders, &seedling_name, &mount_name);

        let mut expected = douglas_folders.application_mounts.clone();
        expected.push("traefik");
        expected.push("shared");
        assert_eq!(result, expected);
    }
}
