use crate::{
    Header, Request, Response, RestClient, RestClientError, ServerClosedConnections,
    SimpleRestClient, StreamedResponse,
};
use async_trait::async_trait;
use hyper::{
    StatusCode,
    rt::{Read, ReadBufCursor, Write},
};
use hyper_util::rt::TokioIo;
use log::Span;
use std::{
    collections::HashSet,
    fmt::Formatter,
    pin::Pin,
    sync::{Arc, LazyLock},
    task::{Context, Poll},
};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use url::Url;

pub struct IoStream {
    stream: TokioIo<tokio_rustls::client::TlsStream<TcpStream>>,
    authority: String,
}

impl Read for IoStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl Write for IoStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl std::fmt::Display for IoStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "({})", self.authority)
    }
}
impl crate::IoStream for IoStream {}

#[derive(Error, Debug)]
pub enum BuilderError {
    #[error("Invalid host name '{0}'")]
    InvalidHostName(String),
    #[error("Failed to connect to '{authority}': {source}")]
    ConnectionError {
        authority: String,
        source: std::io::Error,
    },
    #[error("TLS handshake with '{authority}' failed: {source}")]
    TlsError {
        authority: String,
        source: std::io::Error,
    },
}

impl PartialEq for BuilderError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BuilderError::InvalidHostName(left), BuilderError::InvalidHostName(right)) => {
                left == right
            }
            (
                BuilderError::ConnectionError {
                    authority: left_authority,
                    source: left_source,
                },
                BuilderError::ConnectionError {
                    authority: right_authority,
                    source: right_source,
                },
            ) => {
                left_authority == right_authority
                    && left_source.to_string() == right_source.to_string()
            }
            (
                BuilderError::TlsError {
                    authority: left_authority,
                    source: left_source,
                },
                BuilderError::TlsError {
                    authority: right_authority,
                    source: right_source,
                },
            ) => {
                left_authority == right_authority
                    && left_source.to_string() == right_source.to_string()
            }
            _ => false,
        }
    }
}

static TLS_CONFIG: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
});

pub async fn build_client(
    authority: &str,
    server_closed_connections: ServerClosedConnections,
) -> Result<impl RestClient + use<>, BuilderError> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(443)),
        None => (authority, 443),
    };

    let tcp_stream =
        TcpStream::connect((host, port))
            .await
            .map_err(|source| BuilderError::ConnectionError {
                authority: authority.to_string(),
                source,
            })?;

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| BuilderError::InvalidHostName(host.to_string()))?;

    let connector = TlsConnector::from(Arc::clone(&TLS_CONFIG));
    let tls_stream = connector
        .connect(server_name, tcp_stream)
        .await
        .map_err(|source| BuilderError::TlsError {
            authority: authority.to_string(),
            source,
        })?;

    let io_stream = IoStream {
        stream: TokioIo::new(tls_stream),
        authority: authority.to_string(),
    };

    Ok(SimpleRestClient::new(
        "https",
        authority,
        io_stream,
        vec![Header::new("host", host)],
        server_closed_connections,
    ))
}

#[async_trait]
pub trait RedirectFollowingClient {
    async fn execute(
        &mut self,
        span: &Span,
        initial_authority: &str,
        request: Request,
    ) -> Result<Response, RestClientError>;

    async fn execute_streaming(
        &mut self,
        span: &Span,
        initial_authority: &str,
        request: Request,
        sink: &mut Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    ) -> Result<StreamedResponse, RestClientError>;
}

pub struct TlsRedirectFollowingClient {
    max_redirects: u8,
    server_closed_connections: ServerClosedConnections,
}

impl TlsRedirectFollowingClient {
    pub fn new(max_redirects: u8, server_closed_connections: ServerClosedConnections) -> Self {
        Self {
            max_redirects,
            server_closed_connections,
        }
    }
    fn split_authority_and_path(url: &Url) -> Result<(String, String), RestClientError> {
        let host = url.host_str().ok_or_else(|| {
            RestClientError::General(format!("Redirect location '{url}' has no host"))
        })?;

        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        let mut path = url.path().to_string();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }

        Ok((authority, path))
    }

    fn resolve_redirect(
        authority: &str,
        path: &str,
        headers: &[Header],
    ) -> Result<(String, String), RestClientError> {
        let location = headers
            .iter()
            .find(|header| {
                header
                    .name
                    .eq_ignore_ascii_case(hyper::header::LOCATION.as_str())
            })
            .ok_or_else(|| {
                RestClientError::General("Redirect did not specify a Location header".to_string())
            })?;

        let base = Url::parse(&format!("https://{authority}{path}"))
            .map_err(|err| RestClientError::General(format!("Invalid base url: {err}")))?;
        let target = base
            .join(&location.value)
            .map_err(|err| RestClientError::General(format!("Invalid redirect location: {err}")))?;

        Self::split_authority_and_path(&target)
    }

    fn is_downgrading_redirect(status: StatusCode) -> bool {
        status == StatusCode::MOVED_PERMANENTLY
            || status == StatusCode::FOUND
            || status == StatusCode::SEE_OTHER
    }

    fn is_preserving_redirect(status: StatusCode) -> bool {
        status == StatusCode::TEMPORARY_REDIRECT || status == StatusCode::PERMANENT_REDIRECT
    }

    fn is_redirect(status: StatusCode) -> bool {
        Self::is_downgrading_redirect(status) || Self::is_preserving_redirect(status)
    }

    fn rebuild_request_for_redirect(
        status: hyper::StatusCode,
        request: &Request,
        new_path: String,
    ) -> Request {
        if Self::is_downgrading_redirect(status) {
            return Request::Get {
                path: new_path,
                headers: request.headers(),
            };
        }

        match request {
            Request::Delete { headers, .. } => Request::Delete {
                path: new_path,
                headers: headers.clone(),
            },
            Request::Get { headers, .. } => Request::Get {
                path: new_path,
                headers: headers.clone(),
            },
            Request::Head { headers, .. } => Request::Head {
                path: new_path,
                headers: headers.clone(),
            },
            Request::Post { headers, body, .. } => Request::Post {
                path: new_path,
                headers: headers.clone(),
                body: body.clone(),
            },
            Request::Put { headers, body, .. } => Request::Put {
                path: new_path,
                headers: headers.clone(),
                body: body.clone(),
            },
        }
    }
}

#[async_trait]
impl RedirectFollowingClient for TlsRedirectFollowingClient {
    async fn execute(
        &mut self,
        span: &Span,
        initial_authority: &str,
        request: Request,
    ) -> Result<Response, RestClientError> {
        let mut authority = initial_authority.to_string();
        let mut visited = HashSet::new();
        let mut redirects_followed = 0u8;
        let mut request = request.clone();

        loop {
            if !visited.insert(format!("{authority}{}", request.path())) {
                return Err(RestClientError::General(
                    "Redirect loop detected".to_string(),
                ));
            }

            let mut client = build_client(&authority, self.server_closed_connections)
                .await
                .map_err(|err| RestClientError::General(err.to_string()))?;
            let response = client.execute(span, &request).await?;

            if let Response::Error {
                status, headers, ..
            } = &response
            {
                let status_code = hyper::StatusCode::from_u16(*status)
                    .map_err(|err| RestClientError::General(err.to_string()))?;
                if Self::is_redirect(status_code) && redirects_followed < self.max_redirects {
                    let (new_authority, new_path) =
                        Self::resolve_redirect(&authority, &request.path(), headers)?;
                    authority = new_authority;
                    request = Self::rebuild_request_for_redirect(status_code, &request, new_path);
                    redirects_followed += 1;
                    continue;
                }
            }

            return Ok(response);
        }
    }

    async fn execute_streaming(
        &mut self,
        span: &Span,
        initial_authority: &str,
        request: Request,
        sink: &mut Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    ) -> Result<StreamedResponse, RestClientError> {
        let mut authority = initial_authority.to_string();
        let mut visited = HashSet::new();
        let mut redirects_followed = 0u8;
        let mut request = request.clone();

        loop {
            if !visited.insert(format!("{authority}{}", request.path())) {
                return Err(RestClientError::General(
                    "Redirect loop detected".to_string(),
                ));
            }

            let mut client = build_client(&authority, self.server_closed_connections)
                .await
                .map_err(|err| RestClientError::General(err.to_string()))?;
            let response = client.execute_streaming(span, &request, sink).await?;

            if let StreamedResponse::Error {
                status, headers, ..
            } = &response
            {
                let status_code = hyper::StatusCode::from_u16(*status)
                    .map_err(|err| RestClientError::General(err.to_string()))?;
                if Self::is_redirect(status_code) && redirects_followed < self.max_redirects {
                    let (new_authority, new_path) =
                        Self::resolve_redirect(&authority, &request.path(), headers)?;
                    authority = new_authority;
                    request = Self::rebuild_request_for_redirect(status_code, &request, new_path);
                    redirects_followed += 1;
                    continue;
                }
            }

            return Ok(response);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod redirect_classification {
        use super::*;

        #[test]
        fn test_is_downgrading_redirect_should_match_301_302_303_only() {
            for status in [
                StatusCode::MOVED_PERMANENTLY,
                StatusCode::FOUND,
                StatusCode::SEE_OTHER,
            ] {
                assert!(TlsRedirectFollowingClient::is_downgrading_redirect(status));
            }

            for status in [
                StatusCode::TEMPORARY_REDIRECT,
                StatusCode::PERMANENT_REDIRECT,
                StatusCode::OK,
                StatusCode::NOT_FOUND,
            ] {
                assert!(!TlsRedirectFollowingClient::is_downgrading_redirect(status));
            }
        }

        #[test]
        fn test_is_preserving_redirect_should_match_307_308_only() {
            for status in [
                StatusCode::TEMPORARY_REDIRECT,
                StatusCode::PERMANENT_REDIRECT,
            ] {
                assert!(TlsRedirectFollowingClient::is_preserving_redirect(status));
            }

            for status in [
                StatusCode::MOVED_PERMANENTLY,
                StatusCode::FOUND,
                StatusCode::SEE_OTHER,
                StatusCode::OK,
            ] {
                assert!(!TlsRedirectFollowingClient::is_preserving_redirect(status));
            }
        }

        #[test]
        fn test_is_redirect_should_match_any_redirect_status() {
            for status in [
                StatusCode::MOVED_PERMANENTLY,
                StatusCode::FOUND,
                StatusCode::SEE_OTHER,
                StatusCode::TEMPORARY_REDIRECT,
                StatusCode::PERMANENT_REDIRECT,
            ] {
                assert!(TlsRedirectFollowingClient::is_redirect(status));
            }

            for status in [StatusCode::OK, StatusCode::NOT_FOUND] {
                assert!(!TlsRedirectFollowingClient::is_redirect(status));
            }
        }
    }

    mod split_authority_and_path {
        use super::*;

        #[test]
        fn test_split_authority_and_path_should_include_the_port_when_present() {
            let url = Url::parse("https://example.com:8443/foo/bar").unwrap();

            let (authority, path) =
                TlsRedirectFollowingClient::split_authority_and_path(&url).unwrap();

            assert_eq!(authority, "example.com:8443");
            assert_eq!(path, "/foo/bar");
        }

        #[test]
        fn test_split_authority_and_path_should_omit_the_port_when_default() {
            let url = Url::parse("https://example.com/foo/bar").unwrap();

            let (authority, path) =
                TlsRedirectFollowingClient::split_authority_and_path(&url).unwrap();

            assert_eq!(authority, "example.com");
            assert_eq!(path, "/foo/bar");
        }

        #[test]
        fn test_split_authority_and_path_should_include_the_query_string() {
            let url = Url::parse("https://example.com/foo?a=1&b=2").unwrap();

            let (_, path) = TlsRedirectFollowingClient::split_authority_and_path(&url).unwrap();

            assert_eq!(path, "/foo?a=1&b=2");
        }
    }

    mod resolve_redirect {
        use super::*;

        #[test]
        fn test_resolve_redirect_should_resolve_a_relative_location() {
            let headers = vec![Header::new("location", "/other")];

            let (authority, path) =
                TlsRedirectFollowingClient::resolve_redirect("example.com", "/foo", &headers)
                    .unwrap();

            assert_eq!(authority, "example.com");
            assert_eq!(path, "/other");
        }

        #[test]
        fn test_resolve_redirect_should_resolve_an_absolute_cross_host_location() {
            let headers = vec![Header::new(
                "location",
                "https://cdn.example.com/blobs/abc123",
            )];

            let (authority, path) =
                TlsRedirectFollowingClient::resolve_redirect("example.com", "/foo", &headers)
                    .unwrap();

            assert_eq!(authority, "cdn.example.com");
            assert_eq!(path, "/blobs/abc123");
        }

        #[test]
        fn test_resolve_redirect_should_match_the_location_header_case_insensitively() {
            let headers = vec![Header::new("Location", "/other")];

            let result =
                TlsRedirectFollowingClient::resolve_redirect("example.com", "/foo", &headers);

            assert!(result.is_ok());
        }

        #[test]
        fn test_resolve_redirect_should_error_when_no_location_header_is_present() {
            let result = TlsRedirectFollowingClient::resolve_redirect("example.com", "/foo", &[]);

            assert!(result.is_err());
        }
    }

    mod rebuild_request_for_redirect {
        use super::*;

        #[test]
        fn test_rebuild_request_for_redirect_should_downgrade_a_301_post_to_a_get() {
            let request = Request::Post {
                path: "/foo".to_string(),
                headers: vec![Header::new("x-test", "1")],
                body: Some("body".to_string()),
            };

            let result = TlsRedirectFollowingClient::rebuild_request_for_redirect(
                hyper::StatusCode::MOVED_PERMANENTLY,
                &request,
                "/bar".to_string(),
            );

            assert_eq!(
                result,
                Request::Get {
                    path: "/bar".to_string(),
                    headers: vec![Header::new("x-test", "1")],
                }
            );
        }

        #[test]
        fn test_rebuild_request_for_redirect_should_preserve_a_307_post_method_and_body() {
            let request = Request::Post {
                path: "/foo".to_string(),
                headers: vec![Header::new("x-test", "1")],
                body: Some("body".to_string()),
            };

            let result = TlsRedirectFollowingClient::rebuild_request_for_redirect(
                hyper::StatusCode::TEMPORARY_REDIRECT,
                &request,
                "/bar".to_string(),
            );

            assert_eq!(
                result,
                Request::Post {
                    path: "/bar".to_string(),
                    headers: vec![Header::new("x-test", "1")],
                    body: Some("body".to_string()),
                }
            );
        }

        #[test]
        fn test_rebuild_request_for_redirect_should_preserve_a_get() {
            let request = Request::Get {
                path: "/foo".to_string(),
                headers: vec![],
            };

            let result = TlsRedirectFollowingClient::rebuild_request_for_redirect(
                hyper::StatusCode::FOUND,
                &request,
                "/bar".to_string(),
            );

            assert_eq!(
                result,
                Request::Get {
                    path: "/bar".to_string(),
                    headers: vec![],
                }
            );
        }
    }

    mod builder_error {
        use super::*;

        #[test]
        fn test_eq_should_treat_same_invalid_host_name_as_equal() {
            assert_eq!(
                BuilderError::InvalidHostName("bad host".to_string()),
                BuilderError::InvalidHostName("bad host".to_string())
            );
        }

        #[test]
        fn test_eq_should_treat_different_variants_as_unequal() {
            let left = BuilderError::InvalidHostName("bad host".to_string());
            let right = BuilderError::ConnectionError {
                authority: "bad host".to_string(),
                source: std::io::Error::other("boom"),
            };

            assert_ne!(left, right);
        }

        #[test]
        fn test_eq_should_compare_connection_errors_by_authority_and_source_message() {
            let left = BuilderError::ConnectionError {
                authority: "example.com".to_string(),
                source: std::io::Error::other("boom"),
            };
            let right = BuilderError::ConnectionError {
                authority: "example.com".to_string(),
                source: std::io::Error::other("boom"),
            };

            assert_eq!(left, right);
        }
    }
}
