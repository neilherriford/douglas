use hyper::client::conn::http1::SendRequest;

use crate::io_builder::IoStream;
use std::error::Error;

#[async_trait::async_trait]
pub trait HttpExecutorBuilder {
    async fn build(
        &self,
        io_stream: Box<dyn IoStream + Send + 'static>,
    ) -> Result<Box<dyn HttpExecutor>, Box<dyn Error>>;
}

pub struct HttpClientBuilder {}

impl HttpClientBuilder {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl HttpExecutorBuilder for HttpClientBuilder {
    async fn build(
        &self,
        io_stream: Box<dyn IoStream + Send + 'static>,
    ) -> Result<Box<dyn HttpExecutor>, Box<dyn Error>> {
        let (sender, conn) = hyper::client::conn::http1::handshake(io_stream).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                eprintln!("Connection error: {:?}", e);
            }
        });

        Ok(Box::new(HttpClient { sender }))
    }
}

#[async_trait::async_trait]
pub(crate) trait HttpExecutor {
    async fn execute(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>>;
}

pub(crate) struct HttpClient {
    sender: SendRequest<String>,
}

#[async_trait::async_trait]
impl HttpExecutor for HttpClient {
    async fn execute(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>> {
        let res = self.sender.send_request(req).await?;
        Ok(res)
    }
}
