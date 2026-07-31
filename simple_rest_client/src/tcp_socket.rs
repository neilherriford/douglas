use crate::{Header, RestClient, ServerClosedConnections, SimpleRestClient};
use hyper_util::rt::TokioIo;
use std::fmt::Formatter;
use thiserror::Error;

use hyper::rt::{Read, ReadBufCursor, Write};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::TcpStream;

pub struct IoStream {
    stream: TokioIo<TcpStream>,
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
    #[error("Failed to connect to '{authority}': {source}")]
    ConnectionError {
        authority: String,
        source: std::io::Error,
    },
}

impl PartialEq for BuilderError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
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
        }
    }
}

pub async fn build_client(
    host: &str,
    port: u16,
    server_closed_connections: ServerClosedConnections,
) -> Result<impl RestClient + use<>, BuilderError> {
    let authority = format!("{host}:{port}");
    let tcp_stream =
        TcpStream::connect((host, port))
            .await
            .map_err(|source| BuilderError::ConnectionError {
                authority: authority.clone(),
                source,
            })?;

    let io_stream = IoStream {
        stream: TokioIo::new(tcp_stream),
        authority: authority.clone(),
    };

    Ok(SimpleRestClient::new(
        "http",
        &authority,
        io_stream,
        vec![Header::new("host", host)],
        server_closed_connections,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod builder_error {
        use super::*;

        #[test]
        fn test_eq_should_compare_connection_errors_by_authority_and_source_message() {
            let left = BuilderError::ConnectionError {
                authority: "127.0.0.1:5000".to_string(),
                source: std::io::Error::other("foo"),
            };
            let right = BuilderError::ConnectionError {
                authority: "127.0.0.1:5000".to_string(),
                source: std::io::Error::other("foo"),
            };

            assert_eq!(left, right);
        }

        #[test]
        fn test_eq_should_treat_different_authorities_as_unequal() {
            let left = BuilderError::ConnectionError {
                authority: "127.0.0.1:5000".to_string(),
                source: std::io::Error::other("foo"),
            };
            let right = BuilderError::ConnectionError {
                authority: "127.0.0.1:5001".to_string(),
                source: std::io::Error::other("foo"),
            };

            assert_ne!(left, right);
        }

        #[test]
        fn test_eq_should_treat_different_source_messages_as_unequal() {
            let left = BuilderError::ConnectionError {
                authority: "127.0.0.1:5000".to_string(),
                source: std::io::Error::other("foo"),
            };
            let right = BuilderError::ConnectionError {
                authority: "127.0.0.1:5000".to_string(),
                source: std::io::Error::other("bar"),
            };

            assert_ne!(left, right);
        }
    }
}
