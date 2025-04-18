use crate::http_executor::{HttpExecutor, HttpExecutorBuilder};
use crate::io_builder::IoStream;
use crate::log::Logger;
use crate::request_builder::RequestBuilder;
use http_body_util::BodyExt;
use hyper::{StatusCode, body::Incoming};
use std::error::Error;
use std::future::Future;
use std::sync::Arc;

pub mod http_executor;
pub mod io_builder;
pub mod log;
pub mod request_builder;
pub mod unix_domain_socket_io_builder;

pub enum Response {
    Okay(Option<String>),
    Created(Option<String>),
    NoContent,
    Error { status: u16, message: String },
}

pub trait RestClient {
    fn get(&mut self, path: &str) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
    fn put(
        &mut self,
        path: &str,
        body: &str,
    ) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
    fn post(
        &mut self,
        path: &str,
        body: &str,
    ) -> impl Future<Output = Result<Response, Box<dyn Error>>>;
}

pub struct SimpleRestClient {
    logger: Arc<dyn Logger>,
    http_executor: Box<dyn HttpExecutor>,
    request_builder: Box<dyn RequestBuilder>,
}

impl SimpleRestClient {
    pub async fn build(
        logger: Arc<dyn Logger>,
        http_executor_builder: Box<dyn HttpExecutorBuilder>,
        io_stream: Box<dyn IoStream>,
        request_builder: Box<dyn RequestBuilder>,
    ) -> Result<Self, Box<dyn Error>> {
        let http_executor = http_executor_builder
            .build(Arc::clone(&logger), io_stream)
            .await?;

        Ok(Self {
            logger: Arc::clone(&logger),
            http_executor,
            request_builder,
        })
    }

    async fn read_body(
        &self,
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
        perform_result: Result<hyper::Response<Incoming>, Box<dyn Error>>,
    ) -> Result<Response, Box<dyn Error>> {
        match perform_result {
            Ok(res) => {
                let status = res.status();
                let body = self.read_body(res).await?;
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

impl RestClient for SimpleRestClient {
    async fn get(&mut self, path: &str) -> Result<Response, Box<dyn Error>> {
        let req = self.request_builder.build("GET", path, None).unwrap();

        let perform_result = self.http_executor.execute(req).await;
        self.create_response(perform_result).await
    }

    async fn put(&mut self, path: &str, body: &str) -> Result<Response, Box<dyn Error>> {
        let req = self.request_builder.build("PUT", path, Some(body)).unwrap();

        let perform_result = self.http_executor.execute(req).await;
        self.create_response(perform_result).await
    }

    async fn post(&mut self, path: &str, body: &str) -> Result<Response, Box<dyn Error>> {
        let req = self
            .request_builder
            .build("POST", path, Some(body))
            .unwrap();

        let perform_result = self.http_executor.execute(req).await;
        self.create_response(perform_result).await
    }
}
