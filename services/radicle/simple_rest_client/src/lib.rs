pub mod log;
pub mod unix_domain_socket;

use crate::log::Logger;
use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use std::convert::TryFrom;
use std::error::Error;
use std::sync::Arc;

pub enum Request {
    Delete {
        path: String,
        headers: Option<Vec<Header>>,
    },
    Get {
        path: String,
        headers: Option<Vec<Header>>,
    },
    Post {
        path: String,
        headers: Option<Vec<Header>>,
        body: Option<String>,
    },
    Put {
        path: String,
        headers: Option<Vec<Header>>,
        body: Option<String>,
    },
}

pub enum Response<T: TryFrom<String>> {
    Okay {
        headers: Vec<Header>,
        body: Option<T>,
    },
    Created {
        headers: Vec<Header>,
        body: Option<T>,
    },
    NoContent {
        headers: Vec<Header>,
    },
    Error {
        headers: Vec<Header>,
        status: u16,
        body: Option<T>,
    },
}

#[derive(Debug)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

pub trait IoStream: hyper::rt::Read + hyper::rt::Write + Unpin + Send + Sync {}

#[async_trait::async_trait]
pub trait RestClient<T: TryFrom<String>> {
    async fn execute(&mut self, request: &Request) -> Result<Response<T>, Box<dyn Error>>;
}

pub struct SimpleRestClient<TIo: IoStream> {
    scheme: String,
    authority: String,
    io_stream: Option<TIo>,
    sender: Option<SendRequest<String>>,
    default_headers: Option<Vec<Header>>,
    logger: Arc<dyn Logger>,
}

impl<TIo: IoStream + 'static> SimpleRestClient<TIo> {
    pub fn new(
        scheme: &str,
        authority: &str,
        io_stream: TIo,
        default_headers: Option<Vec<Header>>,
        logger: Arc<dyn Logger>,
    ) -> Self {
        Self {
            scheme: scheme.into(),
            authority: authority.into(),
            io_stream: Some(io_stream),
            sender: None,
            default_headers,
            logger,
        }
    }

    fn build_hyper_request(
        &self,
        request: &Request,
    ) -> Result<hyper::Request<String>, Box<dyn Error>> {
        let uri_builder = hyper::http::uri::Builder::new()
            .scheme(self.scheme.as_str())
            .authority(self.authority.as_str());
        let mut request_builer = hyper::Request::builder();
        let request_headers: &Option<Vec<Header>>;
        let request_body: &Option<String>;

        match request {
            Request::Delete { path, headers } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builer = request_builer.method("DELETE").uri(uri);
                request_headers = headers;
                request_body = &None;
            }
            Request::Get { path, headers } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builer = request_builer.method("GET").uri(uri);
                request_headers = headers;
                request_body = &None;
            }
            Request::Post {
                path,
                headers,
                body,
            } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builer = request_builer.method("POST").uri(uri);
                request_body = body;
                request_headers = headers;
            }
            Request::Put {
                path,
                headers,
                body,
            } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builer = request_builer.method("PUT").uri(uri);
                request_body = body;
                request_headers = headers;
            }
        }

        for headers in vec![&self.default_headers, request_headers] {
            if let Some(headers) = headers {
                for header in headers {
                    request_builer =
                        request_builer.header(header.name.clone(), header.value.clone());
                }
            }
        }

        Ok(request_builer.body(request_body.clone().unwrap_or("".to_string()))?)
    }

    async fn send_request<TResponseBody>(
        &mut self,
        request: hyper::Request<String>,
    ) -> Result<Response<TResponseBody>, Box<dyn Error>>
    where
        TResponseBody: TryFrom<String> + std::fmt::Display,
        TResponseBody::Error: std::fmt::Debug,
    {
        if self.sender.is_none() {
            let io = self.io_stream.take().ok_or("IO stream already taken")?;

            let (sender, conn) = hyper::client::conn::http1::handshake::<TIo, String>(io).await?;
            let task_logger = Arc::clone(&self.logger);
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    task_logger.error(&format!("Connection error: {:?}", e));
                }
            });

            self.sender = Some(sender);
        }

        let response = self.sender.as_mut().unwrap().send_request(request).await?;
        Ok(self.build_response_from_hyper_response(response).await?)
    }

    async fn build_response_from_hyper_response<TResponseBody>(
        &self,
        hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Response<TResponseBody>, Box<dyn Error>>
    where
        TResponseBody: TryFrom<String> + std::fmt::Display,
        TResponseBody::Error: std::fmt::Debug,
    {
        let status = hyper_response.status();
        let headers: Vec<Header> = hyper_response
            .headers()
            .iter()
            .map(|(header_name, header_value)| Header {
                name: header_name.to_string(),
                value: header_value.to_str().unwrap().to_string(),
            })
            .collect();
        let body = self.read_body(hyper_response).await?;

        match status {
            hyper::StatusCode::OK => Ok(Response::<TResponseBody>::Okay { headers, body }),
            hyper::StatusCode::CREATED => Ok(Response::<TResponseBody>::Created { headers, body }),
            hyper::StatusCode::NO_CONTENT => Ok(Response::<TResponseBody>::NoContent { headers }),
            _ => Ok(Response::<TResponseBody>::Error {
                headers,
                status: status.as_u16(),
                body,
            }),
        }
    }

    async fn read_body<TResponseBody>(
        &self,
        mut hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Option<TResponseBody>, Box<dyn Error>>
    where
        TResponseBody: TryFrom<String> + std::fmt::Display,
        TResponseBody::Error: std::fmt::Debug,
    {
        let mut body = String::new();

        while let Some(next) = hyper_response.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                body.push_str(String::from_utf8(chunk.to_vec()).unwrap().as_str());
            }
        }

        if body.len() == 0 {
            Ok(None)
        } else {
            Ok(Some(TResponseBody::try_from(body).unwrap()))
        }
    }

    fn pretty_headers(&self, headers: &Vec<Header>) -> String {
        headers
            .iter()
            .map(|header| format!("'{}={}'", header.name, header.value))
            .collect::<Vec<String>>()
            .join(", ")
    }

    fn log_request(&self, request: &Request) {
        let verb: &str;
        let request_path: &String;
        let request_headers: &Option<Vec<Header>>;
        let request_body: &Option<String>;

        match request {
            Request::Delete { path, headers } => {
                verb = "DELETE";
                request_path = path;
                request_headers = headers;
                request_body = &None;
            }
            Request::Get { path, headers } => {
                verb = "GET";
                request_path = path;
                request_headers = headers;
                request_body = &None;
            }
            Request::Post {
                path,
                headers,
                body,
            } => {
                verb = "POST";
                request_path = path;
                request_headers = headers;
                request_body = body;
            }
            Request::Put {
                path,
                headers,
                body,
            } => {
                verb = "PUT";
                request_path = path;
                request_headers = headers;
                request_body = body;
            }
        };

        let headers = if let Some(headers) = request_headers {
            self.pretty_headers(&headers)
        } else {
            "".to_string()
        };

        let mut result = format!(
            "Performing '{}' on '{}://{}{}', with headers {}",
            verb, self.scheme, self.authority, request_path, headers
        );

        if let Some(body) = request_body {
            result.push_str(&format!(", with body {}", body));
        }

        self.logger.info(&result);
    }

    fn log_response<TResponseBody>(&self, response: &Response<TResponseBody>)
    where
        TResponseBody: TryFrom<String> + std::fmt::Display,
        TResponseBody::Error: std::fmt::Debug,
    {
        let status_code: u16;
        let response_headers: &Vec<Header>;
        let response_body: &Option<TResponseBody>;

        match response {
            Response::Okay { headers, body } => {
                response_headers = headers;
                response_body = body;
                status_code = 200;
            }
            Response::Created { headers, body } => {
                response_headers = headers;
                response_body = body;
                status_code = 201;
            }
            Response::NoContent { headers } => {
                response_headers = headers;
                response_body = &None;
                status_code = 202;
            }
            Response::Error {
                headers,
                status,
                body,
            } => {
                response_headers = headers;
                response_body = body;
                status_code = *status;
            }
        }

        let mut result = format!(
            "Received '{}' with headers {}",
            status_code,
            self.pretty_headers(response_headers)
        );

        if let Some(body) = response_body {
            result.push_str(&format!(", with body {}", body));
        }

        self.logger.info(&result);
    }
}

#[async_trait::async_trait]
impl<TResponseBody, TIo> RestClient<TResponseBody> for SimpleRestClient<TIo>
where
    TResponseBody: TryFrom<String> + std::fmt::Display,
    TResponseBody::Error: std::fmt::Debug,
    TIo: IoStream + 'static,
{
    async fn execute(
        &mut self,
        request: &Request,
    ) -> Result<Response<TResponseBody>, Box<dyn Error>> {
        self.log_request(request);
        let hyper_request = self.build_hyper_request(&request)?;
        let response: Response<TResponseBody> = self.send_request(hyper_request).await?;
        self.log_response(&response);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::MockLogger;
    use hyper_util::rt::TokioIo;
    use mockall::predicate::str::contains;
    use tokio_test::io::Mock;

    impl IoStream for TokioIo<Mock> {}

    #[tokio::test]
    async fn logs_successful_request() {
        let io = tokio_test::io::Builder::new()
            .write(b"GET HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();

        logger
            .expect_info()
            .with(contains(
                "Performing 'GET' on 'HTTP://localhost/', with headers",
            ))
            .times(1)
            .return_const(());

        logger
            .expect_info()
            .with(contains("Received '200' with headers 'content-length=0'"))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: None,
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        assert!(matches!(
            res,
            Response::Okay {
                headers: _,
                body: None
            }
        ));

        if let Response::Okay { headers, body: _ } = res {
            assert_eq!(headers.len(), 1);
            assert_eq!(headers[0].name, "content-length");
            assert_eq!(headers[0].value, "0");
        }
    }

    #[tokio::test]
    async fn logs_connection_failures() {
        let io = tokio_test::io::Builder::new()
            .write(b"GET HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .read_error(std::io::Error::new(std::io::ErrorKind::Other, "oops"))
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();

        logger
            .expect_info()
            .with(contains(
                "Performing 'GET' on 'HTTP://localhost/', with headers",
            ))
            .times(1)
            .return_const(());

        logger
            .expect_info()
            .with(contains("Received '200' with headers 'content-length=0'"))
            .times(1)
            .return_const(());

        logger
            .expect_error()
            .withf(|msg| msg.contains("Connection error") && msg.contains("oops"))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let _: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: None,
            })
            .await;
    }

    #[tokio::test]
    async fn returns_createds() {
        let io = tokio_test::io::Builder::new()
            .write(b"GET HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 201 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger
            .expect_info()
            .with(contains(
                "Performing 'GET' on 'HTTP://localhost/', with headers",
            ))
            .times(1)
            .return_const(());

        logger
            .expect_info()
            .with(contains(
                "Received '201' with headers 'content-length=11', with body hello world",
            ))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: None,
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::Created {
                headers,
                body: Some(body),
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "11");
                assert_eq!(body, "hello world");
            }
            _ => panic!("Expected body and headers to be present"),
        }
    }

    #[tokio::test]
    async fn returns_but_no_content() {
        let io = tokio_test::io::Builder::new()
            .write(b"GET HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 204 OK\r\nContent-Length: 0\r\n\r\n")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger.expect_info().times(2).return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: None,
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::NoContent { headers } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "0");
            }
            _ => panic!("Expected headers to be present"),
        }
    }

    #[tokio::test]
    async fn considers_any_other_status_to_be_error() {
        let io = tokio_test::io::Builder::new()
            .write(b"GET HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 400 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger.expect_info().times(2).return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: None,
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::Error {
                headers,
                status,
                body: Some(body),
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "11");
                assert_eq!(status, 400u16);
                assert_eq!(body, "hello world");
            }
            _ => panic!("Expected headers, body and status to be present"),
        }
    }

    #[tokio::test]
    async fn deletes() {
        let io = tokio_test::io::Builder::new()
            .write(b"DELETE HTTP://localhost")
            .write(b"/ HTTP/1.1\r\n\r\n")
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger.expect_info().times(2).return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Delete {
                path: "/".to_string(),
                headers: None,
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::Okay {
                headers,
                body: Some(body),
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "11");
                assert_eq!(body, "hello world");
            }
            _ => panic!("Expected body and headers to be present"),
        }
    }

    #[tokio::test]
    async fn puts() {
        let io = tokio_test::io::Builder::new()
            .write(b"PUT HTTP://localhost")
            .write(b"/ HTTP/1.1\r\ncontent-length: 4\r\n\r\nbody")
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger.expect_info().times(2).return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, None, Arc::new(logger));

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Put {
                path: "/".to_string(),
                headers: None,
                body: Some("body".to_string()),
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::Okay {
                headers,
                body: Some(body),
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "11");
                assert_eq!(body, "hello world");
            }
            _ => panic!("Expected headers and body to be present"),
        }
    }

    #[tokio::test]
    async fn posts() {
        let io = tokio_test::io::Builder::new()
            .write(b"POST HTTP://localhost")
            .write(b"/ HTTP/1.1\r\nfoo: bar\r\nbaz: qux\r\ncontent-length: 4\r\n\r\nbody")
            .read(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .build();
        let io = TokioIo::new(io);

        let mut logger = MockLogger::new();
        logger.expect_info().times(2).return_const(());

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            Some(vec![Header::new("foo", "bar")]),
            Arc::new(logger),
        );

        let result: Result<Response<String>, Box<dyn std::error::Error>> = client
            .execute(&Request::Post {
                path: "/".to_string(),
                headers: Some(vec![Header::new("baz", "qux")]),
                body: Some("body".to_string()),
            })
            .await;

        assert_eq!(result.is_ok(), true);

        let res = result.unwrap();

        match res {
            Response::Okay {
                headers,
                body: Some(body),
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].name, "content-length");
                assert_eq!(headers[0].value, "11");

                assert_eq!(body, "hello world");
            }
            _ => panic!("Expected headers and body to be present"),
        }
    }
}
