use super::assert_non_empty_string_argument;
use crate::{Container, DockerError, Label, deserialize_labels, serialize_labels};
use log::{Reporter, Span};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::{
    assert_created_with_body, assert_okay, assert_okay_with_body,
};
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Header, Request, Response, RestClient};
use std::sync::Arc;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Network {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Labels")]
    #[serde(deserialize_with = "deserialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConnectedContainers {
    #[serde(rename = "Containers")]
    #[serde(deserialize_with = "deserialize_keys")]
    pub container_ids: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct CreationBody {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Labels")]
    #[serde(serialize_with = "serialize_labels")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreationResponse {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct ConnectionBody {
    #[serde(rename = "Container")]
    container_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConnectionError {
    message: String,
}

pub(crate) fn deserialize_keys<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let json: Json = Json::deserialize(deserializer)?;
    let obj = json
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("Expected Containers to be an object"))?;

    Ok(obj.keys().map(|key| key.as_str().to_string()).collect())
}

pub struct NetworkCommand {
    reporter: Arc<dyn Reporter>,
    rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
}

impl NetworkCommand {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: Arc<tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>>,
        parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
        }
    }

    pub async fn inspect_by_id(&mut self, id: &str) -> Result<Network, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Inspect network by id",
            log::ScopeKind::Task,
        )
        .start_guard();

        assert_non_empty_string_argument("id", id)?;
        guard.finish(
            self.inspect_network_by_hight::<Network>(guard.span(), id)
                .await,
        )
    }

    pub async fn inspect_by_name(&mut self, name: &str) -> Result<Network, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Inspect network by name",
            log::ScopeKind::Task,
        )
        .start_guard();
        assert_non_empty_string_argument("name", name)?;
        guard.finish(
            self.inspect_network_by_hight::<Network>(guard.span(), name)
                .await,
        )
    }

    pub async fn find_connected_containers_by_id(
        &mut self,
        network_id: &str,
    ) -> Result<Vec<String>, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find connected networks by id",
            log::ScopeKind::Task,
        )
        .start_guard();
        assert_non_empty_string_argument("network_id", network_id)?;
        guard.finish(
            self.find_connected_containers_by_hight(guard.span(), network_id)
                .await,
        )
    }

    pub async fn find_connected_containers_by_name(
        &mut self,
        network_name: &str,
    ) -> Result<Vec<String>, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Find connected containers by name",
            log::ScopeKind::Task,
        )
        .start_guard();
        assert_non_empty_string_argument("network_name", network_name)?;
        guard.finish(
            self.find_connected_containers_by_hight(guard.span(), network_name)
                .await,
        )
    }

    pub async fn create(&mut self, name: &str, labels: Vec<Label>) -> Result<Network, DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Create network",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: "/networks/create".to_string(),
            body: Some(serde_json::to_string(&CreationBody {
                name: name.to_string(),
                labels,
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(guard.span(), &req).await?
        };

        let body = assert_created_with_body(response)?;
        let json = self.parser.parse(body)?;
        let result: CreationResponse = from_value(json)?;

        guard.finish(
            self.inspect_network_by_hight(guard.span(), &result.id)
                .await,
        )
    }

    pub async fn connect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Connect to network",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: format!("/networks/{}/connect", network.id),
            body: Some(serde_json::to_string(&ConnectionBody {
                container_id: container.id.to_string(),
            })?),
            headers: vec![Header::content_type_json()],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(guard.span(), &req).await?;
        guard.finish(assert_okay(response))?;
        Ok(())
    }

    pub async fn disconnect(
        &mut self,
        network: &Network,
        container: &Container,
    ) -> Result<(), DockerError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Disconnect container",
            log::ScopeKind::Task,
        )
        .start_guard();
        let req = Request::Post {
            path: format!("/networks/{}/disconnect", network.id),
            body: Some(serde_json::to_string(&ConnectionBody {
                container_id: container.id.to_string(),
            })?),
            headers: vec![Header::content_type_json()],
        };

        let response = {
            let mut rest_client = self.rest_client.lock().await;
            rest_client.execute(guard.span(), &req).await?
        };

        if !self.is_already_disconnected(&response) {
            assert_okay(response)?;
        }
        guard.finish(Ok(()))
    }

    async fn inspect_network_by_hight<T>(
        &mut self,
        span: &Span,
        hight: &str,
    ) -> Result<T, DockerError>
    where
        T: DeserializeOwned,
    {
        let request = Request::Get {
            path: format!("/networks/{}", hight),
            headers: vec![],
        };

        let mut rest_client = self.rest_client.lock().await;
        let response = rest_client.execute(span, &request).await?;
        let body = assert_okay_with_body(response)?;
        let json = self.parser.parse(body)?;

        Ok(from_value(json)?)
    }

    async fn find_connected_containers_by_hight(
        &mut self,
        span: &Span,
        hight: &str,
    ) -> Result<Vec<String>, DockerError> {
        let network = self
            .inspect_network_by_hight::<ConnectedContainers>(span, hight)
            .await?;

        Ok(network.container_ids)
    }

    fn is_already_disconnected(&mut self, response: &Response) -> bool {
        if let Response::Error {
            status: 500,
            body: Some(body),
            ..
        } = response
            && let Ok(json) = self.parser.parse(body.to_string())
            && let Ok(connection_error) = from_value::<ConnectionError>(json)
        {
            return connection_error.message.contains("not connected");
        }

        false
    }
}
