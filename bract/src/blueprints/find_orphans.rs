use crate::blueprints::provision_seedling_secrets::seedling_name_from_approle;
use crate::blueprints::{
    TRAEFIK_SEEDLING_NAME, openbao_socket_path, seedling_name_from_agent_prefixed,
    seedling_name_from_doug_prefixed, traefik_dynamic_dir,
};
use bract_types::Orphans;
use config::DouglasFolders;
use file_system::{FileReader, FileSystemError, Folder};
use identity::Identity;
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
    openbao_client_factory: &dyn openbao::ClientFactory,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
) -> Result<Orphans, FindOrphansError> {
    let seedling_names = seedbank_client.list().await?;
    let seedbank_names = protect_core_seedling_names(
        seedling_names
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );

    let mut known_image_repositories: HashSet<String> = HashSet::new();
    for name in &seedling_names {
        let seedling = seedbank_client.load(name).await?;
        known_image_repositories.insert(seedling.definition.image.formatted_name());
    }

    let live_containers = docker_client.list_containers().await?;
    let container_names = seedling_names_from_live_containers(&live_containers);

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

    let mount_names = list_mount_names(folder, douglas_folders)?;

    let openbao_secret_names = list_openbao_secret_names(
        openbao_client_factory,
        file_reader,
        identity,
        douglas_folders,
    )
    .await;

    Ok(compute_orphans(
        &seedbank_names,
        &known_image_repositories,
        &container_names,
        &network_names,
        &route_file_names,
        &resin_repository_names,
        &mount_names,
        &openbao_secret_names,
    ))
}

fn protect_core_seedling_names(mut names: HashSet<String>) -> HashSet<String> {
    names.insert(TRAEFIK_SEEDLING_NAME.to_string());
    names.insert(openbao::SEEDLING_NAME.to_string());
    names
}

fn seedling_names_from_live_containers(
    live_containers: &[docker_types::ContainerName],
) -> Vec<Name> {
    live_containers
        .iter()
        .filter_map(|container| seedling_name_from_doug_prefixed(container.as_ref()))
        .chain(
            live_containers
                .iter()
                .filter_map(|container| seedling_name_from_agent_prefixed(container.as_ref())),
        )
        .collect::<HashSet<Name>>()
        .into_iter()
        .collect()
}

fn list_mount_names(
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
) -> Result<Vec<Name>, FindOrphansError> {
    let mounts_dir = douglas_folders.seedling_mounts();

    if !folder.exists(&mounts_dir) {
        return Ok(Vec::new());
    }

    Ok(folder
        .entries(&mounts_dir)?
        .iter()
        .filter_map(|entry| entry.name.parse().ok())
        .collect())
}

async fn list_openbao_secret_names(
    openbao_client_factory: &dyn openbao::ClientFactory,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
) -> Vec<Name> {
    let socket_path = openbao_socket_path(douglas_folders);
    let Ok(mut openbao_client) = openbao_client_factory.build(&socket_path).await else {
        return Vec::new();
    };
    let Ok(admin_token) = openbao::app_role::login(
        openbao_client.as_mut(),
        file_reader,
        identity,
        douglas_folders,
    )
    .await
    else {
        return Vec::new();
    };
    let role_names = openbao_client
        .list_auth_roles(&admin_token, &openbao_types::AuthType::AppRole)
        .await
        .unwrap_or_default();
    let policy_names = openbao_client
        .list_policies(&admin_token)
        .await
        .unwrap_or_default();

    role_names
        .iter()
        .chain(policy_names.iter())
        .filter_map(|name| seedling_name_from_approle(name))
        .collect::<HashSet<Name>>()
        .into_iter()
        .collect()
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
    known_image_repositories: &HashSet<String>,
    container_names: &[Name],
    network_names: &[Name],
    route_file_names: &[Name],
    resin_repository_names: &[String],
    mount_names: &[Name],
    openbao_secret_names: &[Name],
) -> Orphans {
    Orphans {
        containers: not_in(seedbank_names, container_names),
        networks: not_in(seedbank_names, network_names),
        route_files: not_in(seedbank_names, route_file_names),
        resin_repositories: resin_repository_names
            .iter()
            .filter(|name| !known_image_repositories.contains(*name))
            .cloned()
            .collect(),
        mounts: not_in(seedbank_names, mount_names),
        openbao_secrets: not_in(seedbank_names, openbao_secret_names),
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

    fn known_repos(values: &[&str]) -> HashSet<String> {
        values
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    }

    #[test]
    fn protect_core_seedling_names_should_add_traefik_and_openbao_even_when_absent() {
        let result = protect_core_seedling_names(seedbank_names(&["hello-world"]));

        assert!(result.contains("hello-world"));
        assert!(result.contains("traefik"));
        assert!(result.contains("openbao"));
    }

    #[test]
    fn seedling_names_from_live_containers_should_map_an_agent_container_back_to_its_seedling() {
        let result = seedling_names_from_live_containers(&["doug-agent.secrets".parse().unwrap()]);

        assert_eq!(result, vec![name("secrets")]);
    }

    #[test]
    fn seedling_names_from_live_containers_should_not_duplicate_a_seedling_with_both_containers() {
        let result = seedling_names_from_live_containers(&[
            "doug.secrets".parse().unwrap(),
            "doug-agent.secrets".parse().unwrap(),
        ]);

        assert_eq!(result, vec![name("secrets")]);
    }

    #[test]
    fn test_compute_orphans_should_never_flag_a_core_seedling_missing_from_seedbank() {
        let seedbank = protect_core_seedling_names(seedbank_names(&[]));
        let repos = known_repos(&[]);

        let result = compute_orphans(
            &seedbank,
            &repos,
            &[name("traefik"), name("openbao")],
            &[name("traefik"), name("openbao")],
            &[name("traefik")],
            &[],
            &[name("openbao")],
            &[],
        );

        assert!(result.containers.is_empty());
        assert!(result.networks.is_empty());
        assert!(result.route_files.is_empty());
        assert!(result.mounts.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_be_empty_when_everything_is_known() {
        let seedbank = seedbank_names(&["hello-world"]);
        let repos = known_repos(&["hello-world"]);

        let result = compute_orphans(
            &seedbank,
            &repos,
            &[name("hello-world")],
            &[name("hello-world")],
            &[name("hello-world")],
            &["hello-world".to_string()],
            &[name("hello-world")],
            &[name("hello-world")],
        );

        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_find_a_container_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(&seedbank, &repos, &[name("stale")], &[], &[], &[], &[], &[]);

        assert_eq!(result.containers, vec![name("stale")]);
        assert!(result.networks.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_find_a_network_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(&seedbank, &repos, &[], &[name("stale")], &[], &[], &[], &[]);

        assert_eq!(result.networks, vec![name("stale")]);
    }

    #[test]
    fn test_compute_orphans_should_find_a_route_file_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(&seedbank, &repos, &[], &[], &[name("stale")], &[], &[], &[]);

        assert_eq!(result.route_files, vec![name("stale")]);
    }

    #[test]
    fn test_compute_orphans_should_find_a_resin_repository_with_no_matching_seedling_image() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(
            &seedbank,
            &repos,
            &[],
            &[],
            &[],
            &["stale".to_string()],
            &[],
            &[],
        );

        assert_eq!(result.resin_repositories, vec!["stale".to_string()]);
    }

    #[test]
    fn test_compute_orphans_should_flag_a_namespaced_resin_repository_as_orphaned() {
        let seedbank = seedbank_names(&["hello-world"]);
        let repos = known_repos(&["hello-world"]);

        let result = compute_orphans(
            &seedbank,
            &repos,
            &[],
            &[],
            &[],
            &["someone/hello-world".to_string()],
            &[],
            &[],
        );

        assert_eq!(
            result.resin_repositories,
            vec!["someone/hello-world".to_string()]
        );
    }

    #[test]
    fn test_compute_orphans_should_not_flag_a_registered_seedlings_own_pulled_image_repository() {
        let seedbank = seedbank_names(&["openbao"]);
        let repos = known_repos(&["openbao/openbao"]);

        let result = compute_orphans(
            &seedbank,
            &repos,
            &[],
            &[],
            &[],
            &["openbao/openbao".to_string()],
            &[],
            &[],
        );

        assert!(result.resin_repositories.is_empty());
    }

    #[test]
    fn test_compute_orphans_should_find_a_mount_dir_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(&seedbank, &repos, &[], &[], &[], &[], &[name("stale")], &[]);

        assert_eq!(result.mounts, vec![name("stale")]);
    }

    #[test]
    fn test_compute_orphans_should_find_an_openbao_secret_with_no_seedbank_record() {
        let seedbank = seedbank_names(&[]);
        let repos = known_repos(&[]);

        let result = compute_orphans(&seedbank, &repos, &[], &[], &[], &[], &[], &[name("stale")]);

        assert_eq!(result.openbao_secrets, vec![name("stale")]);
    }

    fn mock_admin_login(
        openbao_client: &mut openbao::MockClient,
        file_reader: &mut file_system::MockFileReader,
        identity: &mut identity::MockIdentity,
    ) {
        file_reader.expect_exists().returning(|_| true);
        file_reader
            .expect_read_all()
            .returning(|_| Ok("encrypted".to_string()));
        identity
            .expect_decrypt()
            .returning(|_, _| Ok("plain".to_string()));
        openbao_client
            .expect_login()
            .returning(|_, _, _| Ok("admin-token".to_string()));
    }

    #[tokio::test]
    async fn list_openbao_secret_names_should_report_a_role_with_no_matching_policy() {
        let mut file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let mut openbao_client = openbao::MockClient::new();
        mock_admin_login(&mut openbao_client, &mut file_reader, &mut identity);
        openbao_client
            .expect_list_auth_roles()
            .returning(|_, _| Ok(vec!["seedling.hello-openbao".to_string()]));
        openbao_client
            .expect_list_policies()
            .returning(|_| Ok(Vec::new()));

        let mut openbao_client_factory = openbao::MockClientFactory::new();
        openbao_client_factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));

        let result = list_openbao_secret_names(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &config::DouglasFolders::new(),
        )
        .await;

        assert_eq!(result, vec![name("hello-openbao")]);
    }

    #[tokio::test]
    async fn list_openbao_secret_names_should_report_a_policy_with_no_matching_role() {
        let mut file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let mut openbao_client = openbao::MockClient::new();
        mock_admin_login(&mut openbao_client, &mut file_reader, &mut identity);
        openbao_client
            .expect_list_auth_roles()
            .returning(|_, _| Ok(Vec::new()));
        openbao_client
            .expect_list_policies()
            .returning(|_| Ok(vec!["seedling.hello-openbao".to_string()]));

        let mut openbao_client_factory = openbao::MockClientFactory::new();
        openbao_client_factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));

        let result = list_openbao_secret_names(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &config::DouglasFolders::new(),
        )
        .await;

        assert_eq!(result, vec![name("hello-openbao")]);
    }

    #[tokio::test]
    async fn list_openbao_secret_names_should_not_duplicate_a_name_present_in_both() {
        let mut file_reader = file_system::MockFileReader::new();
        let mut identity = identity::MockIdentity::new();
        let mut openbao_client = openbao::MockClient::new();
        mock_admin_login(&mut openbao_client, &mut file_reader, &mut identity);
        openbao_client
            .expect_list_auth_roles()
            .returning(|_, _| Ok(vec!["seedling.hello-openbao".to_string()]));
        openbao_client
            .expect_list_policies()
            .returning(|_| Ok(vec!["seedling.hello-openbao".to_string()]));

        let mut openbao_client_factory = openbao::MockClientFactory::new();
        openbao_client_factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));

        let result = list_openbao_secret_names(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            &config::DouglasFolders::new(),
        )
        .await;

        assert_eq!(result, vec![name("hello-openbao")]);
    }
}
