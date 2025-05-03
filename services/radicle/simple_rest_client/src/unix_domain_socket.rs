use crate::{Header, Logger, Parser, RestClient, SimpleRestClient};
use hyper_util::rt::TokioIo;
use std::error::Error;
use std::fmt::Formatter;
use std::path::Path;
use std::sync::Arc;

use hyper::rt::{Read, ReadBufCursor, Write};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::UnixStream;

pub struct IoStream {
    stream: TokioIo<UnixStream>,
    socket_file_path: String,
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
        write!(f, "({})", self.socket_file_path)
    }
}
impl crate::IoStream for IoStream {}

pub async fn build_client<T>(
    socket_file_path: String,
    logger: Arc<dyn Logger>,
    parser: impl Parser<String, T> + 'static,
) -> Result<impl RestClient<T>, Box<dyn Error>>
where
    T: Send,
{
    let unix_stream = UnixStream::connect(Path::new(&socket_file_path)).await?;
    let io_stream = IoStream {
        stream: TokioIo::new(unix_stream),
        socket_file_path,
    };

    let result = SimpleRestClient::new(
        "http",
        "localhost",
        io_stream,
        Some(vec![Header::new("host", "localhost")]),
        Arc::clone(&logger),
        parser,
    );

    Ok(result)
}
