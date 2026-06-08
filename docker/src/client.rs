use crate::{DockerError, commands::ping};
use async_trait::async_trait;
use simple_rest_client::{RestClient, ServerClosedConnections, unix_domain_socket::build_client};
use std::path::PathBuf;

#[async_trait]
pub trait Ping {
    async fn execute(&mut self, span: &log::Span) -> Result<(), DockerError>;
}

pub struct UdsPing {
    rest_client: Box<dyn RestClient>,
}

impl UdsPing {
    pub async fn build_with_default_socket_path() -> Result<Self, DockerError> {
        Self::build(PathBuf::from("/var/run/docker.sock")).await
    }

    pub async fn build(socket_file_path: PathBuf) -> Result<Self, DockerError> {
        let rest_client = build_client(socket_file_path, ServerClosedConnections::Ignore).await?;
        Ok(Self {
            rest_client: Box::new(rest_client),
        })
    }
}

#[async_trait]
impl Ping for UdsPing {
    async fn execute(&mut self, span: &log::Span) -> Result<(), DockerError> {
        ping::ping(span, &mut *self.rest_client).await
    }
}
