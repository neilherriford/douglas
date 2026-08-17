use crate::{
    DockerError,
    commands::{container, image, json_parser::ChunkedJsonParser, network, system},
};
use async_trait::async_trait;
use docker_types::{
    ContainerId, ContainerName, ContainerSnapshot, ImageDefinition, ImageId, Label, MountName,
    NetworkName, NewContainer, Registry, Status, VersionedImageName,
};
use log::Reporter;
#[cfg(feature = "mock")]
use mockall::automock;
use serde_json::value::Value as Json;
use simple_rest_client::{
    RestClient, ServerClosedConnections,
    parsers::{
        Parser,
        json::{JsonParser, JsonParserError},
    },
    unix_domain_socket::build_client,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

#[cfg_attr(feature = "mock", automock)]
#[async_trait]
pub trait Client: Send + Sync {
    async fn ping(&self) -> Result<(), DockerError>;
    async fn container_labels(
        &self,
        container_ref: ContainerRef,
    ) -> Result<Vec<Label>, DockerError>;
    async fn container_status(&self, container_ref: ContainerRef) -> Result<Status, DockerError>;
    async fn inspect_container(
        &self,
        container_ref: ContainerRef,
        mount_names: &HashMap<PathBuf, MountName>,
    ) -> Result<ContainerSnapshot, DockerError>;
    async fn image_exists(
        &self,
        registry: &Registry,
        image_ref: ImageRef,
    ) -> Result<bool, DockerError>;
    async fn inspect_image(
        &self,
        registry: &Registry,
        image_ref: ImageRef,
    ) -> Result<ImageDefinition, DockerError>;
    async fn pull_image(
        &self,
        registry: &Registry,
        image_name: &VersionedImageName,
    ) -> Result<ImageDefinition, DockerError>;
    async fn container_exists(&self, container_ref: ContainerRef) -> Result<bool, DockerError>;
    async fn start_container(&self, container_ref: ContainerRef) -> Result<(), DockerError>;
    async fn stop_container(&self, container_ref: ContainerRef) -> Result<(), DockerError>;
    async fn delete_container(&self, container_ref: ContainerRef) -> Result<(), DockerError>;
    async fn create_container(
        &self,
        registry: &Registry,
        new_container: &NewContainer,
    ) -> Result<(), DockerError>;
    async fn network_exists(&self, name: &NetworkName) -> Result<bool, DockerError>;
    async fn create_network(&self, name: &NetworkName) -> Result<(), DockerError>;
    async fn connect_network(
        &self,
        network: &NetworkName,
        container_ref: ContainerRef,
    ) -> Result<(), DockerError>;
    async fn disconnect_network(
        &self,
        network: &NetworkName,
        container_ref: ContainerRef,
    ) -> Result<(), DockerError>;
    async fn list_containers(&self) -> Result<Vec<ContainerName>, DockerError>;
    async fn list_networks(&self) -> Result<Vec<NetworkSummary>, DockerError>;
    async fn delete_network(&self, network: &NetworkName) -> Result<(), DockerError>;
}

#[derive(Debug, PartialEq, Clone)]
pub struct NetworkSummary {
    pub id: String,
    pub name: NetworkName,
}

pub struct UdsClient {
    reporter: Arc<dyn Reporter>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    chunked_parser: Arc<dyn Parser<Vec<Json>, ParseError = JsonParserError>>,
    rest_client: Box<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
}

#[cfg_attr(feature = "mock", automock)]
#[async_trait]
pub trait ClientBuilder: Send + Sync {
    async fn build(&self, reporter: Arc<dyn Reporter>) -> Result<Box<dyn Client>, DockerError>;
}

#[derive(Debug, Default)]
pub struct UdsClientBuilder;

#[async_trait]
impl ClientBuilder for UdsClientBuilder {
    async fn build(&self, reporter: Arc<dyn Reporter>) -> Result<Box<dyn Client>, DockerError> {
        let rest_client = build_client(
            PathBuf::from("/var/run/docker.sock"),
            ServerClosedConnections::Ignore,
        )
        .await
        .map_err(|err| DockerError::FailedToCreateClient(err.to_string()))?;

        Ok(Box::new(UdsClient {
            reporter,
            parser: Arc::new(JsonParser::new()),
            chunked_parser: Arc::new(ChunkedJsonParser::new()),
            rest_client: Box::new(tokio::sync::Mutex::new(rest_client)),
        }))
    }
}

pub enum ContainerRef {
    FullName(ContainerName),
    ContainerId(ContainerId),
}

impl std::fmt::Display for ContainerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerRef::FullName(name) => f.write_str(name.as_ref()),
            ContainerRef::ContainerId(id) => f.write_str(&id.to_string()),
        }
    }
}

pub enum ImageRef {
    VersionedName(VersionedImageName),
    ImageId(ImageId),
}

#[async_trait]
impl Client for UdsClient {
    async fn ping(&self) -> Result<(), DockerError> {
        system::ping(Arc::clone(&self.reporter), &*self.rest_client).await
    }

    async fn inspect_container(
        &self,
        container_ref: ContainerRef,
        mount_names: &HashMap<PathBuf, MountName>,
    ) -> Result<ContainerSnapshot, DockerError> {
        container::find(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
            mount_names,
        )
        .await
    }

    async fn inspect_image(
        &self,
        registry: &Registry,
        image_ref: ImageRef,
    ) -> Result<ImageDefinition, DockerError> {
        image::find(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            registry,
            image_ref,
        )
        .await
    }

    async fn pull_image(
        &self,
        registry: &Registry,
        image_name: &VersionedImageName,
    ) -> Result<ImageDefinition, DockerError> {
        image::pull(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            Arc::clone(&self.chunked_parser),
            registry,
            image_name,
        )
        .await
    }

    async fn container_exists(&self, container_ref: ContainerRef) -> Result<bool, DockerError> {
        container::exists(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
        )
        .await
    }

    async fn start_container(&self, container_ref: ContainerRef) -> Result<(), DockerError> {
        container::start(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            container_ref,
        )
        .await
    }

    async fn stop_container(&self, container_ref: ContainerRef) -> Result<(), DockerError> {
        container::stop(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            container_ref,
        )
        .await
    }

    async fn container_labels(
        &self,
        container_ref: ContainerRef,
    ) -> Result<Vec<Label>, DockerError> {
        container::labels(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
        )
        .await
    }

    async fn delete_container(&self, container_ref: ContainerRef) -> Result<(), DockerError> {
        container::delete(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            container_ref,
        )
        .await
    }

    async fn create_container(
        &self,
        registry: &Registry,
        new_container: &NewContainer,
    ) -> Result<(), DockerError> {
        container::create(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            registry,
            new_container,
        )
        .await
    }

    async fn container_status(&self, container_ref: ContainerRef) -> Result<Status, DockerError> {
        container::status(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
        )
        .await
    }

    async fn image_exists(
        &self,
        registry: &Registry,
        image_ref: ImageRef,
    ) -> Result<bool, DockerError> {
        image::exists(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            registry,
            image_ref,
        )
        .await
    }

    async fn network_exists(&self, name: &NetworkName) -> Result<bool, DockerError> {
        network::exists(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            name,
        )
        .await
    }

    async fn create_network(&self, name: &NetworkName) -> Result<(), DockerError> {
        network::create(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            name,
            Vec::new(),
        )
        .await
        .map(|_| ())
    }

    async fn connect_network(
        &self,
        network_name: &NetworkName,
        container_ref: ContainerRef,
    ) -> Result<(), DockerError> {
        let target_network = network::inspect_by_name(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            network_name,
        )
        .await?;
        let container_id = container::find_id(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
        )
        .await?;

        network::connect(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            &target_network.id,
            &container_id,
        )
        .await
    }

    async fn disconnect_network(
        &self,
        network_name: &NetworkName,
        container_ref: ContainerRef,
    ) -> Result<(), DockerError> {
        let target_network = network::inspect_by_name(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            network_name,
        )
        .await?;
        let container_id = container::find_id(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            container_ref,
        )
        .await?;

        network::disconnect(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            &target_network.id,
            &container_id,
        )
        .await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerName>, DockerError> {
        container::list(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
        )
        .await
    }

    async fn list_networks(&self) -> Result<Vec<NetworkSummary>, DockerError> {
        let networks = network::list(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
        )
        .await?;

        Ok(networks
            .into_iter()
            .filter_map(|network| {
                network.name.parse().ok().map(|name| NetworkSummary {
                    id: network.id,
                    name,
                })
            })
            .collect())
    }

    async fn delete_network(&self, network: &NetworkName) -> Result<(), DockerError> {
        let target_network = network::inspect_by_name(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            Arc::clone(&self.parser),
            network,
        )
        .await?;

        network::delete(
            Arc::clone(&self.reporter),
            &*self.rest_client,
            &target_network.id,
        )
        .await
    }
}
