use crate::io_builder::{IoBuilder, IoStream};
use hyper_util::rt::TokioIo;
use std::error::Error;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::net::UnixStream;

pub struct UnixDomainSocketIoBuilder {
    socket_file_path: String,
}

impl UnixDomainSocketIoBuilder {
    pub fn new(socket_file_path: String) -> Self {
        Self { socket_file_path }
    }
}

impl IoBuilder for UnixDomainSocketIoBuilder {
    fn build<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Box<dyn IoStream>, Box<dyn Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let stream = UnixStream::connect(Path::new(&self.socket_file_path)).await?;
            Ok(Box::new(TokioIo::new(stream)) as Box<dyn IoStream>)
        })
    }
}
