use bract_types::container_name;
use config::DouglasFolders;
use file_system::Modes;
use std::path::PathBuf;

pub mod bootstrap;
pub(crate) mod drop_seedling;
pub(crate) mod find_deadwood;
pub(crate) mod new_seedling;
pub(crate) mod openbao_status;
pub(crate) mod provision_seedling_secrets;
pub(crate) mod prune_deadwood;
pub(crate) mod reconcile_seedling;
pub(crate) mod rotate_seedling_logs;
pub(crate) mod start_seedling;
pub(crate) mod stop_seedling;
pub(crate) mod watchdog;
pub(crate) mod write_traefik_routes;

const EXPECTED_MOUNT_MODE: Modes = Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute;
pub(crate) const SYSTEM_NETWORK_NAME: &str = "douglas-system";
#[cfg(target_os = "linux")]
pub(crate) const AGENT_MOUNT_RAM_DISK_SIZE_MB: u32 = 1;
#[cfg(target_os = "macos")]
pub(crate) const AGENT_MOUNT_RAM_DISK_SIZE_MB: u32 = 8;
use config::seedlings::TRAEFIK as TRAEFIK_SEEDLING_NAME;
const TRAEFIK_CONFIG_MOUNT_NAME: &str = "config";
const TRAEFIK_DYNAMIC_DIR_NAME: &str = "dynamic";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestedBy {
    Operator,
    Watchdog,
}

pub(crate) fn core_seedling_forbidden_for(
    origin: Option<seedbank_types::Origin>,
    requested_by: RequestedBy,
) -> bool {
    origin == Some(seedbank_types::Origin::Core) && requested_by == RequestedBy::Operator
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum ContainerPresence {
    #[default]
    Absent,
    Present(docker_types::Status),
}

impl ContainerPresence {
    pub(crate) fn exists(&self) -> bool {
        matches!(self, ContainerPresence::Present(_))
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self,
            ContainerPresence::Present(docker_types::Status::Running)
        )
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(
            self,
            ContainerPresence::Present(
                docker_types::Status::Created
                    | docker_types::Status::Exited
                    | docker_types::Status::Dead
            )
        )
    }
}

pub(crate) async fn observe_container(
    docker_client: &dyn docker::client::Client,
    name: &docker_types::ContainerName,
) -> Result<ContainerPresence, docker::DockerError> {
    if !docker_client
        .container_exists(docker::client::ContainerRef::FullName(name.clone()))
        .await?
    {
        return Ok(ContainerPresence::Absent);
    }

    let status = docker_client
        .container_status(docker::client::ContainerRef::FullName(name.clone()))
        .await?;
    Ok(ContainerPresence::Present(status))
}

pub(crate) async fn build_client<T, BuildErr: std::fmt::Display, E>(
    build: impl std::future::Future<Output = Result<T, BuildErr>>,
    to_error: impl FnOnce(Vec<String>) -> E,
) -> Result<T, E> {
    build.await.map_err(|err| to_error(vec![err.to_string()]))
}

pub fn traefik_dynamic_dir(
    douglas_folders: &DouglasFolders,
) -> Result<PathBuf, seedbank_types::NameParseError> {
    let traefik_name: seedbank_types::Name = TRAEFIK_SEEDLING_NAME.parse()?;
    let config_mount_name: seedbank_types::Name = TRAEFIK_CONFIG_MOUNT_NAME.parse()?;

    let mut dynamic_dir =
        douglas_folders.seedling_mount(traefik_name.as_ref(), config_mount_name.as_ref());
    dynamic_dir.push(TRAEFIK_DYNAMIC_DIR_NAME);
    Ok(dynamic_dir)
}

pub(crate) fn seedling_network_name(
    seedling_name: &seedbank_types::Name,
) -> Result<docker_types::NetworkName, docker_types::DockerNameError> {
    container_name(seedling_name)?.as_ref().parse()
}

pub(crate) fn openbao_socket_path(douglas_folders: &DouglasFolders) -> PathBuf {
    let mut path =
        douglas_folders.seedling_mount(openbao::SEEDLING_NAME, openbao::SOCKET_MOUNT_NAME);
    path.push(openbao::SOCKET_NAME);
    path
}

#[cfg(test)]
mod tests {
    use bract_types::{
        agent_container_name, seedling_name_from_agent_prefixed, seedling_name_from_doug_prefixed,
    };

    use super::*;

    #[test]
    fn test_container_name_should_add_the_douglas_prefix() {
        let seedling_name: seedbank_types::Name = "traefik".parse().unwrap();

        let result = container_name(&seedling_name).expect("should be a valid container name");

        assert_eq!(result.as_ref(), "doug.traefik");
    }

    #[test]
    fn test_seedling_name_from_doug_prefixed_should_strip_the_prefix() {
        assert_eq!(
            seedling_name_from_doug_prefixed("doug.hello-world"),
            Some("hello-world".parse().unwrap())
        );
    }

    #[test]
    fn test_seedling_name_from_doug_prefixed_should_be_none_without_the_prefix() {
        assert_eq!(seedling_name_from_doug_prefixed("hello-world"), None);
    }

    #[test]
    fn test_seedling_name_from_doug_prefixed_should_be_none_for_an_invalid_name() {
        assert_eq!(seedling_name_from_doug_prefixed("doug.Not Valid!"), None);
    }

    #[test]
    fn test_seedling_network_name_should_reuse_the_container_name() {
        let seedling_name: seedbank_types::Name = "hello-world".parse().unwrap();

        let result = seedling_network_name(&seedling_name).expect("should be a valid network name");

        assert_eq!(result.as_ref(), "doug.hello-world");
    }

    #[test]
    fn test_agent_container_name_should_add_the_agent_prefix() {
        let seedling_name: seedbank_types::Name = "secrets".parse().unwrap();

        let result =
            agent_container_name(&seedling_name).expect("should be a valid container name");

        assert_eq!(result.as_ref(), "doug-agent.secrets");
    }

    #[test]
    fn test_seedling_name_from_agent_prefixed_should_strip_the_prefix() {
        assert_eq!(
            seedling_name_from_agent_prefixed("doug-agent.secrets"),
            Some("secrets".parse().unwrap())
        );
    }

    #[test]
    fn test_seedling_name_from_agent_prefixed_should_be_none_without_the_prefix() {
        assert_eq!(seedling_name_from_agent_prefixed("secrets"), None);
    }

    #[test]
    fn test_agent_and_doug_prefixes_should_never_collide() {
        let seedling_name: seedbank_types::Name = "secrets".parse().unwrap();
        let agent_name = agent_container_name(&seedling_name).unwrap();
        let app_name = container_name(&seedling_name).unwrap();

        assert_eq!(seedling_name_from_doug_prefixed(agent_name.as_ref()), None);
        assert_eq!(seedling_name_from_agent_prefixed(app_name.as_ref()), None);
    }

    #[test]
    fn test_traefik_dynamic_dir_should_nest_under_traefiks_config_mount() {
        let douglas_folders = DouglasFolders::new();

        let result =
            traefik_dynamic_dir(&douglas_folders).expect("should build a valid dynamic dir path");

        let mut expected = douglas_folders.seedling_mounts();
        expected.push("traefik");
        expected.push("config");
        expected.push("dynamic");
        assert_eq!(result, expected);
    }

    fn present(status: docker_types::Status) -> ContainerPresence {
        ContainerPresence::Present(status)
    }

    #[test]
    fn test_container_presence_should_default_to_absent() {
        assert_eq!(ContainerPresence::default(), ContainerPresence::Absent);
    }

    #[test]
    fn test_container_presence_absent_should_be_neither_existing_running_nor_stopped() {
        let absent = ContainerPresence::Absent;

        assert!(!absent.exists());
        assert!(!absent.is_running());
        assert!(!absent.is_stopped());
    }

    #[test]
    fn test_container_presence_running_should_exist_and_be_running_but_not_stopped() {
        let running = present(docker_types::Status::Running);

        assert!(running.exists());
        assert!(running.is_running());
        assert!(!running.is_stopped());
    }

    #[test]
    fn test_container_presence_should_call_created_exited_and_dead_stopped() {
        for status in [
            docker_types::Status::Created,
            docker_types::Status::Exited,
            docker_types::Status::Dead,
        ] {
            let stopped = present(status);

            assert!(stopped.exists());
            assert!(stopped.is_stopped());
            assert!(!stopped.is_running());
        }
    }

    #[test]
    fn test_container_presence_should_call_paused_restarting_and_removing_neither_running_nor_stopped()
     {
        for status in [
            docker_types::Status::Paused,
            docker_types::Status::Restarting,
            docker_types::Status::Removing,
        ] {
            let in_between = present(status);

            assert!(in_between.exists());
            assert!(!in_between.is_running());
            assert!(!in_between.is_stopped());
        }
    }

    #[tokio::test]
    async fn test_observe_container_should_not_ask_for_the_status_of_a_container_that_is_absent() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_container_exists()
            .returning(|_| Ok(false));
        docker_client.expect_container_status().times(0);
        let name = container_name(&"hello-world".parse().unwrap()).unwrap();

        let result = observe_container(&docker_client, &name).await;

        assert!(matches!(result, Ok(ContainerPresence::Absent)));
    }

    #[tokio::test]
    async fn test_observe_container_should_carry_the_status_of_a_container_that_exists() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_container_exists()
            .returning(|_| Ok(true));
        docker_client
            .expect_container_status()
            .returning(|_| Ok(docker_types::Status::Paused));
        let name = container_name(&"hello-world".parse().unwrap()).unwrap();

        let result = observe_container(&docker_client, &name).await;

        assert!(matches!(
            result,
            Ok(ContainerPresence::Present(docker_types::Status::Paused))
        ));
    }

    #[tokio::test]
    async fn test_observe_container_should_pass_on_a_docker_error() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_container_exists()
            .returning(|_| Err(docker::DockerError::ResourceNotFound));
        let name = container_name(&"hello-world".parse().unwrap()).unwrap();

        let result = observe_container(&docker_client, &name).await;

        assert!(result.is_err());
    }
}
