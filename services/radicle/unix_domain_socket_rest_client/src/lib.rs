mod request_builder;

use http_body_util::BodyExt;
use hyper::StatusCode;
use hyper::body::Incoming;
use request_builder::{LocalhostRequestBuilder, RequestBuilder};
use std::error::Error;
use std::future::Future;

#[async_trait::async_trait]
trait HttpExecutor {
    async fn perform(
        &self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error + Send + Sync>>;
}

struct UnixDomainSocketHttpExecutor {
    socket_file_path: String,
}

#[async_trait::async_trait]
impl HttpExecutor for UnixDomainSocketHttpExecutor {
    async fn perform(
        &self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error + Send + Sync>> {
        use hyper_util::rt::TokioIo;
        use std::path::Path;
        use tokio::net::UnixStream;

        let stream = UnixStream::connect(Path::new(&self.socket_file_path)).await?;
        let io = TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                eprintln!("Connection error: {:?}", e);
            }
        });

        let res = sender.send_request(req).await?;
        Ok(res)
    }
}

pub enum Response {
    Okay(Option<String>),
    Created(Option<String>),
    NoContent,
    Error { status: u16, message: String },
}

pub trait RestClient {
    fn get(&self, path: String) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
    fn put(
        &self,
        path: String,
        body: String,
    ) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
    fn post(
        &self,
        path: String,
        body: String,
    ) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
}

pub struct UnixDomainSocketRestClient {
    http_executor: Box<dyn HttpExecutor>,
    request_builder: Box<dyn RequestBuilder>,
}

impl UnixDomainSocketRestClient {
    pub fn new(socket_file_path: String) -> UnixDomainSocketRestClient {
        UnixDomainSocketRestClient {
            http_executor: Box::new(UnixDomainSocketHttpExecutor { socket_file_path }),
            request_builder: Box::new(LocalhostRequestBuilder {}),
        }
    }

    async fn read_body(
        mut res: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        let mut body = String::new();

        while let Some(next) = res.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                body.push_str(String::from_utf8(chunk.to_vec()).unwrap().as_str());
            }
        }

        if body.len() == 0 {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }

    async fn create_response(
        &self,
        perform_result: Result<hyper::Response<Incoming>, Box<dyn Error + Send + Sync>>,
    ) -> Result<Response, Box<dyn Error>> {
        match perform_result {
            Ok(res) => {
                let status = res.status();
                let body = UnixDomainSocketRestClient::read_body(res).await?;
                let result = match status {
                    StatusCode::OK => Response::Okay(body),
                    StatusCode::CREATED => Response::Created(body),
                    StatusCode::NO_CONTENT => Response::NoContent,
                    status => Response::Error {
                        status: status.as_u16(),
                        message: body.unwrap_or(String::new()),
                    },
                };
                Ok(result)
            }
            Err(err) => return Err(err),
        }
    }
}

impl RestClient for UnixDomainSocketRestClient {
    async fn get(&self, path: String) -> Result<Response, Box<dyn Error>> {
        let req = self
            .request_builder
            .build(String::from("GET"), path, None)
            .unwrap();

        let perform_result = self.http_executor.perform(req).await;
        self.create_response(perform_result).await
    }

    async fn put(&self, path: String, body: String) -> Result<Response, Box<dyn Error>> {
        let req = self
            .request_builder
            .build(String::from("PUT"), path, Some(body))
            .unwrap();

        let perform_result = self.http_executor.perform(req).await;
        self.create_response(perform_result).await
    }

    async fn post(&self, path: String, body: String) -> Result<Response, Box<dyn Error>> {
        let req = self
            .request_builder
            .build(String::from("POST"), path, Some(body))
            .unwrap();

        let perform_result = self.http_executor.perform(req).await;
        self.create_response(perform_result).await
    }
}
