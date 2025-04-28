use crate::{Header, IoStream, Logger, RestClient, SimpleRestClient};
use hyper_util::rt::TokioIo;
use std::error::Error;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixStream;

impl IoStream for TokioIo<UnixStream> {}

pub async fn build_stream(socket_file_path: String) -> Result<impl IoStream, Box<dyn Error>> {
    let stream = UnixStream::connect(Path::new(&socket_file_path)).await?;
    Ok(TokioIo::new(stream))
}

pub async fn build_client<T>(
    socket_path: String,
    logger: Arc<dyn Logger>,
) -> Result<impl RestClient<T>, Box<dyn std::error::Error>>
where
    T: TryFrom<String> + std::fmt::Display,
    T::Error: Debug,
{
    let io_stream = build_stream(socket_path).await?;
    let result = SimpleRestClient::new(
        "http",
        "localhost",
        io_stream,
        Some(vec![Header::new("host", "localhost")]),
        Arc::clone(&logger),
    );

    Ok(result)
}
