use crate::{Header, Reconnect, RestClient, ServerClosedConnections, SimpleRestClient};
use hyper_util::rt::TokioIo;
use std::fmt::Formatter;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

use hyper::rt::{Read, ReadBufCursor, Write};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::UnixStream;

pub struct IoStream {
    stream: TokioIo<UnixStream>,
    socket_file_path: PathBuf,
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
        write!(
            f,
            "({})",
            self.socket_file_path.to_str().ok_or(std::fmt::Error)?
        )
    }
}
impl crate::IoStream for IoStream {}

#[derive(Error, Debug)]
pub enum BuilderError {
    #[error("Socket file not found")]
    SocketFileNotFound,
    #[error("Permission denied trying to open socket")]
    PermissionDenied,
    #[error("Connection refused")]
    ConnectionRefused,
    #[error("General Error: {0}")]
    General(std::io::Error),
}

impl PartialEq for BuilderError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BuilderError::SocketFileNotFound, BuilderError::SocketFileNotFound)
            | (BuilderError::PermissionDenied, BuilderError::PermissionDenied)
            | (BuilderError::ConnectionRefused, BuilderError::ConnectionRefused) => true,
            (BuilderError::General(left), BuilderError::General(right)) => {
                left.to_string() == right.to_string()
            }
            _ => false,
        }
    }
}

impl From<std::io::Error> for BuilderError {
    fn from(value: std::io::Error) -> Self {
        match value.kind() {
            std::io::ErrorKind::NotFound => BuilderError::SocketFileNotFound,
            std::io::ErrorKind::PermissionDenied => BuilderError::PermissionDenied,
            std::io::ErrorKind::ConnectionRefused => BuilderError::ConnectionRefused,
            _ => BuilderError::General(value),
        }
    }
}

struct UnixSocketReconnect {
    socket_file_path: PathBuf,
}

#[async_trait::async_trait]
impl Reconnect<IoStream> for UnixSocketReconnect {
    async fn connect(&self) -> std::io::Result<IoStream> {
        let unix_stream = UnixStream::connect(&self.socket_file_path).await?;
        Ok(IoStream {
            stream: TokioIo::new(unix_stream),
            socket_file_path: self.socket_file_path.clone(),
        })
    }
}

pub async fn build_client(
    socket_file_path: PathBuf,
    server_closed_connections: ServerClosedConnections,
) -> Result<impl RestClient, BuilderError>
where
{
    let unix_stream = UnixStream::connect(socket_file_path.as_path()).await?;
    let io_stream = IoStream {
        stream: TokioIo::new(unix_stream),
        socket_file_path: socket_file_path.clone(),
    };
    let reconnect = Arc::new(UnixSocketReconnect { socket_file_path });

    let result = SimpleRestClient::new(
        "http",
        "localhost",
        io_stream,
        reconnect,
        vec![Header::new("host", "localhost")],
        server_closed_connections,
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod builder_error {
        use super::*;

        #[test]
        fn test_eq_should_treat_same_named_variants_as_equal() {
            assert_eq!(
                BuilderError::SocketFileNotFound,
                BuilderError::SocketFileNotFound
            );
            assert_eq!(
                BuilderError::PermissionDenied,
                BuilderError::PermissionDenied
            );
            assert_eq!(
                BuilderError::ConnectionRefused,
                BuilderError::ConnectionRefused
            );
        }

        #[test]
        fn test_eq_should_treat_different_variants_as_unequal() {
            assert_ne!(
                BuilderError::SocketFileNotFound,
                BuilderError::PermissionDenied
            );
        }

        #[test]
        fn test_eq_should_compare_general_errors_by_source_message() {
            let left = BuilderError::General(std::io::Error::other("foo"));
            let right = BuilderError::General(std::io::Error::other("foo"));

            assert_eq!(left, right);
        }

        #[test]
        fn test_eq_should_treat_different_general_messages_as_unequal() {
            let left = BuilderError::General(std::io::Error::other("oops"));
            let right = BuilderError::General(std::io::Error::other("uhoh"));

            assert_ne!(left, right);
        }

        #[test]
        fn test_from_io_error_should_map_not_found_to_socket_file_not_found() {
            let source = std::io::Error::new(std::io::ErrorKind::NotFound, "foo");

            assert_eq!(BuilderError::from(source), BuilderError::SocketFileNotFound);
        }

        #[test]
        fn test_from_io_error_should_map_permission_denied() {
            let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "foo");

            assert_eq!(BuilderError::from(source), BuilderError::PermissionDenied);
        }

        #[test]
        fn test_from_io_error_should_map_connection_refused() {
            let source = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "foo");

            assert_eq!(BuilderError::from(source), BuilderError::ConnectionRefused);
        }

        #[test]
        fn test_from_io_error_should_map_other_kinds_to_general() {
            let source = std::io::Error::other("foo");

            assert_eq!(
                BuilderError::from(source),
                BuilderError::General(std::io::Error::other("foo"))
            );
        }
    }
}
