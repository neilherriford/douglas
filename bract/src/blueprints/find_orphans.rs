use crate::blueprints::{seedling_name_from_doug_prefixed, traefik_dynamic_dir};
use bract_types::Orphans;
use config::DouglasFolders;
use file_system::{FileSystemError, Folder};
use seedbank_types::{Name, NameParseError};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FindOrphansError {
    #[error("Seedbank error: {0}")]
    Seedbank(#[from] seedbank_client::Error),
    #[error("Docker error: {0}")]
    Docker(#[from] docker::DockerError),
    #[error("Resin error: {0}")]
    Resin(#[from] resin_client::Error),
    #[error("Name parse error: {0}")]
    NameParse(#[from] NameParseError),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
}

pub async fn execute(
    seedbank_client: &dyn seedbank_client::Client,
    docker_client: &dyn docker::client::Client,
    resin_client: &mut dyn resin_client::Client,
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
) -> Result<Orphans, FindOrphansError> {
    let seedbank_names: HashSet<String> = seedbank_client
        .list()
        .await?
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    let container_names: Vec<Name> = docker_client
        .list_containers()
        .await?
        .iter()
        .filter_map(|name| seedling_name_from_doug_prefixed(name.as_ref()))
        .collect();

    let network_names: Vec<Name> = docker_client
        .list_networks()
        .await?
        .iter()
        .filter_map(|network| seedling_name_from_doug_prefixed(network.name.as_ref()))
        .collect();

    let route_file_names = list_route_file_names(folder, douglas_folders)?;

    let resin_repository_names: Vec<String> = resin_client
        .list_repositories()
        .await?
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    Ok(compute_orphans(
        &seedbank_names,
        &container_names,
        &network_names,
        &route_file_names,
        &resin_repository_names,
    ))
}

fn list_route_file_names(
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
) -> Result<Vec<Name>, FindOrphansError> {
    let dynamic_dir = traefik_dynamic_dir(douglas_folders)?;

    if !folder.exists(&dynamic_dir) {
        return Ok(Vec::new());
    }

    Ok(folder
        .entries(&dynamic_dir)?
        .iter()
        .filter_map(|entry| entry.name.strip_suffix(".yml"))
        .filter_map(|name| name.parse().ok())
        .collect())
}

fn compute_orphans(
    seedbank_names: &HashSet<String>,
    container_names: &[Name],
    network_names: &[Name],
    route_file_names: &[Name],
    resin_repository_names: &[String],
) -> Orphans {
    Orphans {
        containers: not_in(seedbank_names, container_names),
        networks: not_in(seedbank_names, network_names),
        route_files: not_in(seedbank_names, route_file_names),
        resin_repositories: resin_repository_names
            .iter()
            .filter(|name| !seedbank_names.contains(*name))
            .cloned()
            .collect(),
    }
}

fn not_in(seedbank_names: &HashSet<String>, candidates: &[Name]) -> Vec<Name> {
    candidates
        .iter()
        .filter(|name| !seedbank_names.contains(&name.to_string()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> Name {
        value.parse().expect("valid name")
    }

    fn seedbank_names(values: &[&str]) -> HashSet<String> {
        values
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    }

    #[test]
    fn test_compute_orphans_should_be_empty_when_everything_is_known() {
        let seedbank = seedbank_names(&["hello-world"]);

        let result = compute_orphans(
            &seedbank,
            &[name("hello-world")],
            &[name("hello-world")],
            &[name("hello-world")],
            &["hello-world".to_string()],
        );

        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_find_a_container_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);

        let result = compute_orphans(&seedbank, &[name("stale")], &[], &[], &[]);

        assert_eq!(result.containers, vec![name("stale")]);
        assert!(result.networks.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_find_a_network_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);

        let result = compute_orphans(&seedbank, &[], &[name("stale")], &[], &[]);

        assert_eq!(result.networks, vec![name("stale")]);
    }

    #[test]
    fn test_compute_orphans_should_find_a_route_file_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);

        let result = compute_orphans(&seedbank, &[], &[], &[name("stale")], &[]);

        assert_eq!(result.route_files, vec![name("stale")]);
    }

    #[test]
    fn test_compute_orphans_should_find_a_resin_repository_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);

        let result = compute_orphans(&seedbank, &[], &[], &[], &["stale".to_string()]);

        assert_eq!(result.resin_repositories, vec!["stale".to_string()]);
    }

    #[test]
    fn test_compute_orphans_should_flag_a_namespaced_resin_repository_as_orphaned() {
        let seedbank = seedbank_names(&["hello-world"]);

        let result = compute_orphans(
            &seedbank,
            &[],
            &[],
            &[],
            &["someone/hello-world".to_string()],
        );

        assert_eq!(
            result.resin_repositories,
            vec!["someone/hello-world".to_string()]
        );
    }
}
