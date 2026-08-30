use crate::blueprints::{container_name, seedling_network_name, traefik_dynamic_dir};
use crate::rolodex::{Rolodex, RolodexError};
use config::DouglasFolders;
use docker::client::ContainerRef;
use docker_types::DockerNameError;
use file_system::{FileSystemError, FileWriter, Folder, Modes, Permissions};
use log::{Reporter, ScopeKind, Span};
use seedbank_types::{Name, NameParseError, PortSpec, RouteSpec, Routing};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;

const TRAEFIK_SEEDLING_NAME: &str = "traefik";

#[derive(Error, Debug)]
pub enum WriteTraefikRoutesError {
    #[error("Seedbank error: {0}")]
    Seedbank(#[from] seedbank_client::Error),
    #[error("Name parse error: {0}")]
    NameParse(#[from] NameParseError),
    #[error("Docker name error: {0}")]
    DockerName(#[from] DockerNameError),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
    #[error("YAML serialization error: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("Docker error: {0}")]
    Docker(#[from] docker::DockerError),
    #[error("Rolodex error: {0}")]
    Rolodex(#[from] RolodexError),
    #[error("Traefik has no service account yet")]
    MissingServiceAccount,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct DynamicConfig {
    http: Http,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct Http {
    routers: HashMap<String, Router>,
    services: HashMap<String, Service>,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct Router {
    rule: String,
    #[serde(rename = "entryPoints")]
    entry_points: Vec<String>,
    service: String,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct Service {
    #[serde(rename = "loadBalancer")]
    load_balancer: LoadBalancer,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct LoadBalancer {
    servers: Vec<LoadBalancerServer>,
}

#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
struct LoadBalancerServer {
    url: String,
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    seedbank_client: &dyn seedbank_client::Client,
    docker_client: &dyn docker::client::Client,
    folder: &dyn Folder,
    file_writer: &dyn FileWriter,
    permissions: &dyn Permissions,
    rolodex: &dyn Rolodex,
    douglas_folders: &DouglasFolders,
) -> Result<(), WriteTraefikRoutesError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Writing traefik dynamic routes",
        ScopeKind::Task,
    )
    .start_guard();

    let result = write_routes(
        seedbank_client,
        docker_client,
        folder,
        file_writer,
        permissions,
        rolodex,
        douglas_folders,
    )
    .await;

    match result {
        Ok(()) => guard.finish(Ok(())),
        Err(err) => guard.finish(Err(err)),
    }
}

async fn write_routes(
    seedbank_client: &dyn seedbank_client::Client,
    docker_client: &dyn docker::client::Client,
    folder: &dyn Folder,
    file_writer: &dyn FileWriter,
    permissions: &dyn Permissions,
    rolodex: &dyn Rolodex,
    douglas_folders: &DouglasFolders,
) -> Result<(), WriteTraefikRoutesError> {
    let traefik_name: Name = TRAEFIK_SEEDLING_NAME.parse()?;
    let traefik_container = container_name(&traefik_name)?;

    let traefik_service_account = rolodex
        .find_service_account(traefik_name.as_ref())?
        .ok_or(WriteTraefikRoutesError::MissingServiceAccount)?;

    let dynamic_dir = traefik_dynamic_dir(douglas_folders)?;
    folder.create_recursively(&dynamic_dir)?;
    permissions.change_user_and_group_ownership(
        &dynamic_dir,
        &traefik_service_account.user.system_name,
        &traefik_service_account.group.system_name,
    )?;
    permissions.change_mode(
        &dynamic_dir,
        &Modes::InheritedOwnerReadWriteExecuteGroupReadWriteExecute,
    )?;

    for name in seedbank_client.list().await? {
        let seedling = seedbank_client.load(&name).await?;

        let Routing::Routed { route, ports } = &seedling.definition.routing else {
            continue;
        };

        // Don't route prior to reconciliation
        let seedling_network = seedling_network_name(&name)?;
        if !docker_client.network_exists(&seedling_network).await? {
            continue;
        }

        let container = container_name(&name)?;
        let contents = render_route(&name, route, ports, container.as_ref())?;

        let mut path = dynamic_dir.clone();
        path.push(format!("{name}.yml"));
        file_writer.write_all(&path, &contents)?;
        permissions.change_user_and_group_ownership(
            &path,
            &traefik_service_account.user.system_name,
            &traefik_service_account.group.system_name,
        )?;
        permissions.change_mode(&path, &Modes::OwnerReadWriteGroupRead)?;

        docker_client
            .connect_network(
                &seedling_network,
                ContainerRef::FullName(traefik_container.clone()),
                None,
            )
            .await?;
    }

    Ok(())
}

fn render_route(
    name: &Name,
    route: &RouteSpec,
    ports: &PortSpec,
    container: &str,
) -> Result<String, serde_yaml_ng::Error> {
    let rule = match route {
        RouteSpec::Root => "Host(`localhost`)".to_string(),
        RouteSpec::Subdomain => format!("Host(`{name}.localhost`)"),
    };

    let config = DynamicConfig {
        http: Http {
            routers: HashMap::from([(
                name.to_string(),
                Router {
                    rule,
                    entry_points: vec!["web".to_string()],
                    service: name.to_string(),
                },
            )]),
            services: HashMap::from([(
                name.to_string(),
                Service {
                    load_balancer: LoadBalancer {
                        servers: vec![LoadBalancerServer {
                            url: format!("http://{container}:{}", ports.public),
                        }],
                    },
                },
            )]),
        },
    };

    serde_yaml_ng::to_string(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_route_should_produce_a_root_router_and_service() {
        let name: Name = "hello-world".parse().expect("valid name");
        let ports = PortSpec {
            public: 3000,
            additional: Vec::new(),
        };

        let result = render_route(&name, &RouteSpec::Root, &ports, "doug.hello-world")
            .expect("should serialize");

        assert_eq!(
            result,
            "http:\n  routers:\n    hello-world:\n      rule: Host(`localhost`)\n      entryPoints:\n      - web\n      service: hello-world\n  services:\n    hello-world:\n      loadBalancer:\n        servers:\n        - url: http://doug.hello-world:3000\n"
        );
    }

    #[test]
    fn test_render_route_should_produce_a_subdomain_host_rule() {
        let name: Name = "second-app".parse().expect("valid name");
        let ports = PortSpec {
            public: 3000,
            additional: Vec::new(),
        };

        let rendered = render_route(&name, &RouteSpec::Subdomain, &ports, "doug.second-app")
            .expect("should serialize");
        let parsed: DynamicConfig = serde_yaml_ng::from_str(&rendered).expect("should parse back");

        assert_eq!(
            parsed
                .http
                .routers
                .get("second-app")
                .expect("router present")
                .rule,
            "Host(`second-app.localhost`)"
        );
    }

    #[test]
    fn test_render_route_should_round_trip_through_yaml() {
        let name: Name = "hello-world".parse().expect("valid name");
        let ports = PortSpec {
            public: 3000,
            additional: Vec::new(),
        };

        let rendered = render_route(&name, &RouteSpec::Root, &ports, "doug.hello-world")
            .expect("should serialize");

        let parsed: DynamicConfig = serde_yaml_ng::from_str(&rendered).expect("should parse back");

        assert_eq!(
            parsed
                .http
                .routers
                .get("hello-world")
                .expect("router present")
                .service,
            "hello-world"
        );
        assert_eq!(
            parsed
                .http
                .services
                .get("hello-world")
                .expect("service present")
                .load_balancer
                .servers[0]
                .url,
            "http://doug.hello-world:3000"
        );
    }
}
