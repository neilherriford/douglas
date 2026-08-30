use crate::{DockerError, Label, to_general_error};
use docker_types::{Ipv4Subnet, NetworkName, serialize_labels};
use log::{Reporter, Span};
use serde::{Deserialize, Serialize};
use serde_json::from_value;
use serde_json::value::Value as Json;
use simple_rest_client::assertions::{
    assert_created_with_body, assert_no_content, assert_okay, assert_okay_with_body,
};
use simple_rest_client::parsers::Parser;
use simple_rest_client::parsers::json::JsonParserError;
use simple_rest_client::{Header, Request, RestClient};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Deserialize, PartialEq)]
pub struct Network {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct IpamConfigEntry {
    #[serde(rename = "Subnet")]
    subnet: String,
    #[serde(rename = "Gateway")]
    gateway: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct Ipam {
    #[serde(rename = "Config")]
    config: Vec<IpamConfigEntry>,
}

#[derive(Debug, Serialize, PartialEq)]
struct CreationBody {
    #[serde(rename = "Name")]
    pub name: NetworkName,
    #[serde(rename = "Labels")]
    #[serde(serialize_with = "serialize_labels")]
    pub labels: Vec<Label>,
    #[serde(rename = "IPAM")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipam: Option<Ipam>,
}

#[derive(Debug, Serialize, PartialEq)]
struct EndpointIpamConfig {
    #[serde(rename = "IPv4Address")]
    ipv4_address: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct EndpointConfig {
    #[serde(rename = "IPAMConfig")]
    ipam_config: EndpointIpamConfig,
}

#[derive(Debug, Serialize, PartialEq)]
struct ConnectionBody {
    #[serde(rename = "Container")]
    container_id: String,
    #[serde(rename = "EndpointConfig")]
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint_config: Option<EndpointConfig>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConnectionError {
    message: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreationResponse {
    #[serde(rename = "Id")]
    id: String,
}

pub async fn inspect_by_name(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    name: &NetworkName,
) -> Result<Network, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Inspect network by name",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Get {
        path: format!("/networks/{}", name.as_ref()),
        headers: vec![],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;

    guard.finish(Ok(from_value(json).map_err(to_general_error)?))
}

pub async fn list(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
) -> Result<Vec<Network>, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Listing networks",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Get {
        path: "/networks".to_string(),
        headers: vec![],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    let body = assert_okay_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;

    guard.finish(Ok(from_value(json).map_err(to_general_error)?))
}

pub async fn delete(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    network_id: &str,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Deleting network",
        log::ScopeKind::Task,
    )
    .start_guard();

    let request = Request::Delete {
        path: format!("/networks/{network_id}"),
        headers: vec![],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &request)
            .await
            .map_err(to_general_error)?
    };
    assert_no_content(response)?;

    guard.finish(Ok(()))
}

pub async fn exists(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    name: &NetworkName,
) -> Result<bool, DockerError> {
    match inspect_by_name(reporter, rest_client, parser, name).await {
        Ok(_) => Ok(true),
        Err(DockerError::ResourceNotFound) => Ok(false),
        Err(err) => Err(err),
    }
}

pub async fn create(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    name: &NetworkName,
    labels: Vec<Label>,
    subnet: Option<&Ipv4Subnet>,
) -> Result<String, DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Create network",
        log::ScopeKind::Task,
    )
    .start_guard();

    let ipam = subnet.map(|subnet| Ipam {
        config: vec![IpamConfigEntry {
            subnet: subnet.cidr.clone(),
            gateway: subnet.gateway.clone(),
        }],
    });

    let req = Request::Post {
        path: "/networks/create".to_string(),
        body: Some(
            serde_json::to_string(&CreationBody {
                name: name.clone(),
                labels,
                ipam,
            })
            .map_err(to_general_error)?,
        ),
        headers: vec![Header::content_type_json()],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &req)
            .await
            .map_err(to_general_error)?
    };

    let body = assert_created_with_body(response)?;
    let json = parser.parse(body).map_err(to_general_error)?;
    let creation_response: CreationResponse = from_value(json).map_err(to_general_error)?;

    guard.finish(Ok(creation_response.id))
}

pub async fn connect(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    network_id: &str,
    container_id: &str,
    static_ipv4: Option<&str>,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Connect to network",
        log::ScopeKind::Task,
    )
    .start_guard();

    let endpoint_config = static_ipv4.map(|ipv4_address| EndpointConfig {
        ipam_config: EndpointIpamConfig {
            ipv4_address: ipv4_address.to_string(),
        },
    });

    let req = Request::Post {
        path: format!("/networks/{network_id}/connect"),
        body: Some(
            serde_json::to_string(&ConnectionBody {
                container_id: container_id.to_string(),
                endpoint_config,
            })
            .map_err(to_general_error)?,
        ),
        headers: vec![Header::content_type_json()],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &req)
            .await
            .map_err(to_general_error)?
    };

    guard.finish(assert_okay(response).map(|_| ()).map_err(DockerError::from))
}

pub async fn disconnect(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
    parser: Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    network_id: &str,
    container_id: &str,
) -> Result<(), DockerError> {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Disconnect container",
        log::ScopeKind::Task,
    )
    .start_guard();

    let req = Request::Post {
        path: format!("/networks/{network_id}/disconnect"),
        body: Some(
            serde_json::to_string(&ConnectionBody {
                container_id: container_id.to_string(),
                endpoint_config: None,
            })
            .map_err(to_general_error)?,
        ),
        headers: vec![Header::content_type_json()],
        query: HashMap::new(),
    };

    let response = {
        let mut rest_client = rest_client.lock().await;
        rest_client
            .execute(guard.span(), &req)
            .await
            .map_err(to_general_error)?
    };

    if is_already_disconnected(&parser, &response) {
        return guard.finish(Ok(()));
    }

    guard.finish(assert_okay(response).map(|_| ()).map_err(DockerError::from))
}

fn is_already_disconnected(
    parser: &Arc<dyn Parser<Json, ParseError = JsonParserError>>,
    response: &simple_rest_client::Response,
) -> bool {
    if let simple_rest_client::Response::Error {
        status: 500,
        body: Some(body),
        ..
    } = response
        && let Ok(json) = parser.parse(body.to_string())
        && let Ok(connection_error) = from_value::<ConnectionError>(json)
    {
        return connection_error.message.contains("not connected");
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use simple_rest_client::{Response, parsers::json::JsonParser};

    fn parser() -> Arc<dyn Parser<Json, ParseError = JsonParserError>> {
        Arc::new(JsonParser::new())
    }

    #[test]
    fn test_is_already_disconnected_should_be_true_for_a_not_connected_error() {
        let response = Response::Error {
            status: 500,
            headers: Vec::new(),
            body: Some(r#"{"message": "container is not connected to network"}"#.to_string()),
        };

        assert!(is_already_disconnected(&parser(), &response));
    }

    #[test]
    fn test_is_already_disconnected_should_be_false_for_an_unrelated_error() {
        let response = Response::Error {
            status: 500,
            headers: Vec::new(),
            body: Some(r#"{"message": "something else went wrong"}"#.to_string()),
        };

        assert!(!is_already_disconnected(&parser(), &response));
    }

    #[test]
    fn test_is_already_disconnected_should_be_false_for_a_non_500_error() {
        let response = Response::Error {
            status: 404,
            headers: Vec::new(),
            body: Some(r#"{"message": "container is not connected to network"}"#.to_string()),
        };

        assert!(!is_already_disconnected(&parser(), &response));
    }
}
