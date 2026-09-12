use crate::blueprints::{
    agent_container_name, container_name, openbao_socket_path, seedling_network_name,
    traefik_dynamic_dir,
};
use bract_types::Deadwood;
use config::DouglasFolders;
use docker::client::ContainerRef;
use docker_types::DockerNameError;
use file_system::{FileDeleter, FileReader, FileSystemError, FolderDeleter};
use identity::Identity;
use log::{Reporter, ScopeKind, Span};
use seedbank_types::NameParseError;
use std::sync::Arc;
use thiserror::Error;

use config::seedlings::TRAEFIK as TRAEFIK_SEEDLING_NAME;

#[derive(Error, Debug)]
pub enum PruneDeadwoodError {
    #[error("Docker error: {0}")]
    Docker(#[from] docker::DockerError),
    #[error("Docker name error: {0}")]
    DockerName(#[from] DockerNameError),
    #[error("Name parse error: {0}")]
    NameParse(#[from] NameParseError),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
    #[error("Resin error: {0}")]
    Resin(#[from] resin_client::Error),
    #[error("OpenBao error: {0}")]
    OpenBao(#[from] openbao::Error),
    #[error("AppRole login error: {0}")]
    AppRole(#[from] openbao::app_role::AppRoleError),
    #[error("Failed to provision seedling secrets: {0}")]
    ProvisionSeedlingSecrets(
        #[from] crate::blueprints::provision_seedling_secrets::ProvisionSeedlingSecretsError,
    ),
}

pub(crate) struct Dependencies<'a> {
    pub docker_client: &'a dyn docker::client::Client,
    pub resin_client: &'a mut dyn resin_client::Client,
    pub file_deleter: &'a dyn FileDeleter,
    pub folder_deleter: &'a dyn FolderDeleter,
    pub openbao_client_factory: &'a dyn openbao::ClientFactory,
    pub file_reader: &'a dyn FileReader,
    pub identity: &'a mut dyn Identity,
    pub douglas_folders: &'a DouglasFolders,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    deps: Dependencies<'_>,
    deadwood: &Deadwood,
) -> Result<(), PruneDeadwoodError> {
    let guard = Span::new(Arc::clone(&reporter), "Pruning deadwood", ScopeKind::Task).start_guard();

    let result = prune(deps, deadwood).await;

    match result {
        Ok(()) => guard.finish(Ok(())),
        Err(err) => guard.finish(Err(err)),
    }
}

async fn prune(deps: Dependencies<'_>, deadwood: &Deadwood) -> Result<(), PruneDeadwoodError> {
    let Dependencies {
        docker_client,
        resin_client,
        file_deleter,
        folder_deleter,
        openbao_client_factory,
        file_reader,
        identity,
        douglas_folders,
    } = deps;

    for name in &deadwood.containers {
        let container = container_name(name)?;
        let _ = docker_client
            .stop_container(ContainerRef::FullName(container.clone()))
            .await;
        match docker_client
            .delete_container(ContainerRef::FullName(container))
            .await
        {
            Ok(()) | Err(docker::DockerError::ResourceNotFound) => {}
            Err(err) => return Err(err.into()),
        }

        let agent_container = agent_container_name(name)?;
        let _ = docker_client
            .stop_container(ContainerRef::FullName(agent_container.clone()))
            .await;
        match docker_client
            .delete_container(ContainerRef::FullName(agent_container))
            .await
        {
            Ok(()) | Err(docker::DockerError::ResourceNotFound) => {}
            Err(err) => return Err(err.into()),
        }
    }

    for name in &deadwood.networks {
        disconnect_traefik(docker_client, name).await?;
        let network = seedling_network_name(name)?;
        docker_client.delete_network(&network).await?;
    }

    if !deadwood.route_files.is_empty() {
        let dynamic_dir = traefik_dynamic_dir(douglas_folders)?;
        for name in &deadwood.route_files {
            disconnect_traefik(docker_client, name).await?;
            let mut path = dynamic_dir.clone();
            path.push(format!("{name}.yml"));
            file_deleter.delete(&path)?;
        }
    }

    for name in &deadwood.resin_repositories {
        let Ok(resin_name) = name.parse::<resin_types::Name>() else {
            continue;
        };
        resin_client.delete_repository(&resin_name).await?;
    }

    for name in &deadwood.mounts {
        let mounts_dir = douglas_folders.seedling_mounts_dir(name.as_ref());
        folder_deleter.delete(&mounts_dir)?;
    }

    if !deadwood.openbao_secrets.is_empty() {
        let socket_path = openbao_socket_path(douglas_folders);
        let mut openbao_client = openbao_client_factory.build(&socket_path).await?;
        let admin_token = openbao::app_role::login(
            openbao_client.as_mut(),
            file_reader,
            identity,
            douglas_folders,
        )
        .await?;

        for name in &deadwood.openbao_secrets {
            crate::blueprints::provision_seedling_secrets::revoke(
                openbao_client.as_mut(),
                &admin_token,
                name,
            )
            .await?;
        }
    }

    Ok(())
}

async fn disconnect_traefik(
    docker_client: &dyn docker::client::Client,
    seedling_name: &seedbank_types::Name,
) -> Result<(), PruneDeadwoodError> {
    let traefik_name: seedbank_types::Name = TRAEFIK_SEEDLING_NAME.parse()?;
    let traefik_container = container_name(&traefik_name)?;
    let seedling_network = seedling_network_name(seedling_name)?;

    match docker_client
        .disconnect_network(&seedling_network, ContainerRef::FullName(traefik_container))
        .await
    {
        Ok(()) | Err(docker::DockerError::ResourceNotFound) => {}
        Err(err) => return Err(err.into()),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docker::MockClient;
    use file_system::{MockFileDeleter, MockFileReader, MockFolderDeleter};
    use identity::MockIdentity;
    use resin_client::MockClient as MockResinClient;

    fn name(value: &str) -> seedbank_types::Name {
        value.parse().expect("valid name")
    }

    #[tokio::test]
    async fn test_prune_should_stop_and_delete_deadwood_containers() {
        let mut docker_client = MockClient::new();
        docker_client
            .expect_stop_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.stale")
            })
            .returning(|_| Ok(()));
        docker_client
            .expect_delete_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.stale")
            })
            .returning(|_| Ok(()));
        docker_client
            .expect_stop_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug-agent.stale")
            })
            .returning(|_| Err(docker::DockerError::ResourceNotFound));
        docker_client
            .expect_delete_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug-agent.stale")
            })
            .returning(|_| Err(docker::DockerError::ResourceNotFound));

        let deadwood = Deadwood {
            containers: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_delete_a_container_even_when_stop_fails() {
        let mut docker_client = MockClient::new();
        docker_client
            .expect_stop_container()
            .returning(|_| Err(docker::DockerError::ResourceNotFound));
        docker_client
            .expect_delete_container()
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            containers: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_also_stop_and_delete_an_deadwood_agent_container() {
        let mut docker_client = MockClient::new();
        docker_client
            .expect_stop_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.stale")
            })
            .returning(|_| Ok(()));
        docker_client
            .expect_delete_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.stale")
            })
            .returning(|_| Ok(()));
        docker_client
            .expect_stop_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug-agent.stale")
            })
            .returning(|_| Ok(()));
        docker_client
            .expect_delete_container()
            .withf(|container_ref| {
                matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug-agent.stale")
            })
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            containers: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_disconnect_traefik_before_deleting_deadwood_networks() {
        let mut docker_client = MockClient::new();
        docker_client
            .expect_disconnect_network()
            .withf(|network, container_ref| {
                network.as_ref() == "doug.stale"
                    && matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.traefik")
            })
            .returning(|_, _| Ok(()));
        docker_client
            .expect_delete_network()
            .withf(|network| network.as_ref() == "doug.stale")
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            networks: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_disconnect_traefik_before_deleting_deadwood_route_files() {
        let douglas_folders = DouglasFolders::new();
        let expected_path = traefik_dynamic_dir(&douglas_folders)
            .expect("should build a dynamic dir path")
            .join("stale.yml");

        let mut docker_client = MockClient::new();
        docker_client
            .expect_disconnect_network()
            .withf(|network, container_ref| {
                network.as_ref() == "doug.stale"
                    && matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.traefik")
            })
            .returning(|_, _| Ok(()));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_delete()
            .withf(move |path| path == expected_path)
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            route_files: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &file_deleter,
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &douglas_folders,
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_tolerate_traefik_already_disconnected_when_a_name_is_deadwood_as_both_a_network_and_a_route_file()
     {
        let douglas_folders = DouglasFolders::new();
        let expected_path = traefik_dynamic_dir(&douglas_folders)
            .expect("should build a dynamic dir path")
            .join("stale.yml");

        let disconnect_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let disconnect_calls_for_closure = std::sync::Arc::clone(&disconnect_calls);

        let mut docker_client = MockClient::new();
        docker_client
            .expect_disconnect_network()
            .withf(|network, container_ref| {
                network.as_ref() == "doug.stale"
                    && matches!(container_ref, ContainerRef::FullName(name) if name.as_ref() == "doug.traefik")
            })
            .times(2)
            .returning(move |_, _| {
                let call_number =
                    disconnect_calls_for_closure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call_number == 0 {
                    Ok(())
                } else {
                    Err(docker::DockerError::ResourceNotFound)
                }
            });
        docker_client
            .expect_delete_network()
            .withf(|network| network.as_ref() == "doug.stale")
            .returning(|_| Ok(()));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_delete()
            .withf(move |path| path == expected_path)
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            networks: vec![name("stale")],
            route_files: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &docker_client,
                resin_client: &mut MockResinClient::new(),
                file_deleter: &file_deleter,
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &douglas_folders,
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_delete_deadwood_resin_repositories() {
        let mut resin_client = MockResinClient::new();
        resin_client
            .expect_delete_repository()
            .withf(|name| name.to_string() == "stale")
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            resin_repositories: vec!["stale".to_string()],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &MockClient::new(),
                resin_client: &mut resin_client,
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_skip_a_resin_repository_name_that_does_not_parse() {
        let deadwood = Deadwood {
            resin_repositories: vec!["Not-A-Valid-Name!".to_string()],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &MockClient::new(),
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_delete_deadwood_mount_directories() {
        let douglas_folders = DouglasFolders::new();
        let expected_path = douglas_folders.seedling_mounts_dir("stale");

        let mut folder_deleter = MockFolderDeleter::new();
        folder_deleter
            .expect_delete()
            .withf(move |path| path == expected_path)
            .returning(|_| Ok(()));

        let deadwood = Deadwood {
            mounts: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &MockClient::new(),
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &folder_deleter,
                openbao_client_factory: &openbao::MockClientFactory::new(),
                file_reader: &MockFileReader::new(),
                identity: &mut MockIdentity::new(),
                douglas_folders: &douglas_folders,
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_revoke_deadwood_openbao_secrets() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_exists().returning(|_| true);
        file_reader
            .expect_read_all()
            .returning(|_| Ok("encrypted".to_string()));

        let mut identity = MockIdentity::new();
        identity
            .expect_decrypt()
            .returning(|_, _| Ok("plain".to_string()));

        let mut openbao_client_factory = openbao::MockClientFactory::new();
        let mut openbao_client = openbao::MockClient::new();
        openbao_client
            .expect_login()
            .returning(|_, _, _| Ok("admin-token".to_string()));
        openbao_client
            .expect_auth_exists()
            .returning(|_, _, _| Ok(true));
        openbao_client
            .expect_delete_auth()
            .withf(|_, _, name| name == "seedling.stale")
            .returning(|_, _, _| Ok(()));
        openbao_client
            .expect_delete_policy()
            .withf(|_, name| name == "seedling.stale")
            .returning(|_, _| Ok(()));
        openbao_client_factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));

        let deadwood = Deadwood {
            openbao_secrets: vec![name("stale")],
            ..Deadwood::default()
        };

        let result = prune(
            Dependencies {
                docker_client: &MockClient::new(),
                resin_client: &mut MockResinClient::new(),
                file_deleter: &MockFileDeleter::new(),
                folder_deleter: &MockFolderDeleter::new(),
                openbao_client_factory: &openbao_client_factory,
                file_reader: &file_reader,
                identity: &mut identity,
                douglas_folders: &DouglasFolders::new(),
            },
            &deadwood,
        )
        .await;

        assert!(result.is_ok());
    }
}
