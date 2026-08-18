use crate::blueprints::{container_name, seedling_network_name, traefik_dynamic_dir};
use bract_types::Orphans;
use config::DouglasFolders;
use docker::client::ContainerRef;
use docker_types::DockerNameError;
use file_system::{FileDeleter, FileSystemError};
use log::{Reporter, ScopeKind, Span};
use seedbank_types::NameParseError;
use std::sync::Arc;
use thiserror::Error;

const TRAEFIK_SEEDLING_NAME: &str = "traefik";

#[derive(Error, Debug)]
pub enum PruneOrphansError {
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
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    docker_client: &dyn docker::client::Client,
    resin_client: &mut dyn resin_client::Client,
    file_deleter: &dyn FileDeleter,
    douglas_folders: &DouglasFolders,
    orphans: &Orphans,
) -> Result<(), PruneOrphansError> {
    let guard = Span::new(Arc::clone(&reporter), "Pruning orphans", ScopeKind::Task).start_guard();

    let result = prune(
        docker_client,
        resin_client,
        file_deleter,
        douglas_folders,
        orphans,
    )
    .await;

    match result {
        Ok(()) => guard.finish(Ok(())),
        Err(err) => guard.finish(Err(err)),
    }
}

async fn prune(
    docker_client: &dyn docker::client::Client,
    resin_client: &mut dyn resin_client::Client,
    file_deleter: &dyn FileDeleter,
    douglas_folders: &DouglasFolders,
    orphans: &Orphans,
) -> Result<(), PruneOrphansError> {
    for name in &orphans.containers {
        let container = container_name(name)?;
        let _ = docker_client
            .stop_container(ContainerRef::FullName(container.clone()))
            .await;
        docker_client
            .delete_container(ContainerRef::FullName(container))
            .await?;
    }

    for name in &orphans.networks {
        disconnect_traefik(docker_client, name).await?;
        let network = seedling_network_name(name)?;
        docker_client.delete_network(&network).await?;
    }

    if !orphans.route_files.is_empty() {
        let dynamic_dir = traefik_dynamic_dir(douglas_folders)?;
        for name in &orphans.route_files {
            disconnect_traefik(docker_client, name).await?;
            let mut path = dynamic_dir.clone();
            path.push(format!("{name}.yml"));
            file_deleter.delete(&path)?;
        }
    }

    for name in &orphans.resin_repositories {
        let Ok(resin_name) = name.parse::<resin_types::Name>() else {
            continue;
        };
        resin_client.delete_repository(&resin_name).await?;
    }

    Ok(())
}

async fn disconnect_traefik(
    docker_client: &dyn docker::client::Client,
    seedling_name: &seedbank_types::Name,
) -> Result<(), PruneOrphansError> {
    let traefik_name: seedbank_types::Name = TRAEFIK_SEEDLING_NAME.parse()?;
    let traefik_container = container_name(&traefik_name)?;
    let seedling_network = seedling_network_name(seedling_name)?;

    docker_client
        .disconnect_network(&seedling_network, ContainerRef::FullName(traefik_container))
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docker::MockClient;
    use file_system::MockFileDeleter;
    use resin_client::MockClient as MockResinClient;

    fn name(value: &str) -> seedbank_types::Name {
        value.parse().expect("valid name")
    }

    #[tokio::test]
    async fn test_prune_should_stop_and_delete_orphaned_containers() {
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

        let orphans = Orphans {
            containers: vec![name("stale")],
            ..Orphans::default()
        };

        let result = prune(
            &docker_client,
            &mut MockResinClient::new(),
            &MockFileDeleter::new(),
            &DouglasFolders::new(),
            &orphans,
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

        let orphans = Orphans {
            containers: vec![name("stale")],
            ..Orphans::default()
        };

        let result = prune(
            &docker_client,
            &mut MockResinClient::new(),
            &MockFileDeleter::new(),
            &DouglasFolders::new(),
            &orphans,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_disconnect_traefik_before_deleting_orphaned_networks() {
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

        let orphans = Orphans {
            networks: vec![name("stale")],
            ..Orphans::default()
        };

        let result = prune(
            &docker_client,
            &mut MockResinClient::new(),
            &MockFileDeleter::new(),
            &DouglasFolders::new(),
            &orphans,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_disconnect_traefik_before_deleting_orphaned_route_files() {
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

        let orphans = Orphans {
            route_files: vec![name("stale")],
            ..Orphans::default()
        };

        let result = prune(
            &docker_client,
            &mut MockResinClient::new(),
            &file_deleter,
            &douglas_folders,
            &orphans,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_delete_orphaned_resin_repositories() {
        let mut resin_client = MockResinClient::new();
        resin_client
            .expect_delete_repository()
            .withf(|name| name.to_string() == "stale")
            .returning(|_| Ok(()));

        let orphans = Orphans {
            resin_repositories: vec!["stale".to_string()],
            ..Orphans::default()
        };

        let result = prune(
            &MockClient::new(),
            &mut resin_client,
            &MockFileDeleter::new(),
            &DouglasFolders::new(),
            &orphans,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_prune_should_skip_a_resin_repository_name_that_does_not_parse() {
        let orphans = Orphans {
            resin_repositories: vec!["Not-A-Valid-Name!".to_string()],
            ..Orphans::default()
        };

        let result = prune(
            &MockClient::new(),
            &mut MockResinClient::new(),
            &MockFileDeleter::new(),
            &DouglasFolders::new(),
            &orphans,
        )
        .await;

        assert!(result.is_ok());
    }
}
