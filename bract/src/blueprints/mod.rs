use config::DouglasFolders;
use file_system::Modes;
use std::path::PathBuf;

pub mod bootstrap;
pub(crate) mod drop_seedling;
pub(crate) mod reconcile_seedling;
pub(crate) mod start_seedling;
pub(crate) mod stop_seedling;

const EXPECTED_MOUNT_MODE: Modes = Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute;
const CONTAINER_NAME_PREFIX: &str = "doug.";

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
