use crate::{Header, RestClient, ServerClosedConnections, SimpleRestClient};
use hyper_util::rt::TokioIo;
use std::fmt::Formatter;
use std::sync::{Arc, LazyLock};
use thiserror::Error;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use hyper::rt::{Read, ReadBufCursor, Write};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::TcpStream;

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
            ) => left_authority == right_authority && left_source.to_string() == right_source.to_string(),
            (
                BuilderError::TlsError {
                    authority: left_authority,
                    source: left_source,
                },
                BuilderError::TlsError {
                    authority: right_authority,
                    source: right_source,
                },
            ) => left_authority == right_authority && left_source.to_string() == right_source.to_string(),
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
) -> Result<impl RestClient, BuilderError> {
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
