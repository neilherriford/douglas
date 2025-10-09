pub mod unix_domain_socket;
use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use log::Logger;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RestClientError {
    #[error("Stream error: {0}")]
    Stream(#[from] hyper::http::Error),
    #[error("Client error: {0}")]
    Client(#[from] hyper::Error),
    #[error("IO stream already taken")]
    IoStreamAlreadyTaken,
    #[error("General error: {0}")]
    General(String),
}

#[derive(Debug, PartialEq)]
pub enum Request {
    Delete {
        path: String,
        headers: Vec<Header>,
    },
    Get {
        path: String,
        headers: Vec<Header>,
    },
    Post {
        path: String,
        headers: Vec<Header>,
        body: Option<String>,
    },
    Put {
        path: String,
        headers: Vec<Header>,
        body: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
pub enum Response {
    Okay {
        headers: Vec<Header>,
        body: Option<String>,
    },
    Created {
        headers: Vec<Header>,
        body: Option<String>,
    },
    NoContent {
        headers: Vec<Header>,
    },
    Error {
        headers: Vec<Header>,
        status: u16,
        body: Option<String>,
    },
}

#[derive(Debug, PartialEq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn content_type_json() -> Header {
        Header::new("Content-type", "application/json")
    }
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

pub fn create_path_and_query_string(path: &str, parameters: HashMap<&str, &str>) -> String {
    if parameters.is_empty() {
        path.to_string()
    } else {
        let encoded_params = parameters
            .into_iter()
            .map(|(name, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(name),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", path, encoded_params)
    }
}

pub trait IoStream: hyper::rt::Read + hyper::rt::Write + Unpin + Send + Sync {}

#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait::async_trait]
pub trait RestClient: Send + Sync {
    async fn execute(&mut self, request: &Request) -> Result<Response, RestClientError>;
}

#[cfg(feature = "mock")]
impl MockRestClient {
    pub fn expect_rest_call<TPredicate>(&mut self, request_predicate: TPredicate, output: Response)
    where
        TPredicate: Fn(&Request) -> bool + Send + 'static,
    {
        self.expect_execute()
            .withf(move |req| request_predicate(req))
            .times(1)
            .return_once(|_req| Ok(output));
    }

    fn parse_path_and_query(path: String) -> Option<(String, Vec<(String, String)>)> {
        let uri: hyper::Uri = path.parse().unwrap();

        let parts = uri.into_parts().path_and_query.unwrap();

        if let Some(query) = parts.query() {
            let mut parameters: Vec<(String, String)> = query
                .split("&")
                .filter_map(|assignment| {
                    let mut chunks = assignment.split("=");
                    match (chunks.next(), chunks.next()) {
                        (Some(name), Some(value)) => Some((name.to_string(), value.to_string())),
                        (Some(value), _) => Some((String::new(), value.to_string())),
                        _ => None,
                    }
                })
                .collect();

            parameters.sort();

            Some((parts.path().to_string(), parameters))
        } else {
            Some((parts.path().to_string(), vec![]))
        }
    }

    fn paths_equal(expected_path: String, actual_path: String) -> bool {
        let expected = MockRestClient::parse_path_and_query(expected_path);
        let actual = MockRestClient::parse_path_and_query(actual_path);

        match (expected, actual) {
            (Some((expected_path, expected_params)), Some((actual_path, actual_params))) => {
                expected_path == actual_path && expected_params == actual_params
            }
            _ => false,
        }
    }

    pub fn create_get_expectation(&self, path: &str) -> Box<dyn Fn(&Request) -> bool + Send> {
        let path = path.to_string();
        Box::new(move |req: &Request| {
            if let Request::Get {
                path: requested_path,
                ..
            } = req
            {
                MockRestClient::paths_equal(path.clone(), requested_path.to_string())
            } else {
                false
            }
        })
    }

    pub fn expect_get_and_return_okay(&mut self, path: &str, body: Option<String>) {
        self.expect_rest_call(
            self.create_get_expectation(path),
            Response::Okay {
                headers: vec![],
                body,
            },
        )
    }

    pub fn expect_get_and_return_created_with_none(&mut self, path: &str) {
        self.expect_rest_call(
            self.create_get_expectation(path),
            Response::Created {
                headers: vec![],
                body: None,
            },
        )
    }

    pub fn expect_get_and_return_no_content(&mut self, path: &str) {
        self.expect_rest_call(
            self.create_get_expectation(path),
            Response::NoContent { headers: vec![] },
        )
    }

    pub fn expect_get_and_return_not_found(&mut self, path: &str) {
        self.expect_rest_call(
            self.create_get_expectation(path),
            Response::Error {
                headers: vec![],
                status: 404,
                body: None,
            },
        )
    }

    pub fn expect_get_and_return_internal_server_error(&mut self, path: &str) {
        self.expect_rest_call(
            self.create_get_expectation(path),
            Response::Error {
                headers: vec![],
                status: 500,
                body: None,
            },
        )
    }

    pub fn create_post_expectation(
        &self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
    ) -> Box<dyn Fn(&Request) -> bool + Send> {
        let expected_path = path.to_string();
        Box::new(move |req: &Request| {
            if let Request::Post {
                path: requested_path,
                body: actual_body,
                headers: actual_headers,
            } = req
            {
                body == *actual_body
                    && MockRestClient::paths_equal(
                        expected_path.clone(),
                        requested_path.to_string(),
                    )
                    && *actual_headers == headers
            } else {
                false
            }
        })
    }

    pub fn expect_post_and_return_okay(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
        response_body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::Okay {
                headers: vec![],
                body: response_body,
            },
        )
    }

    pub fn expect_post_and_return_created(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
        response_body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::Created {
                headers: vec![],
                body: response_body,
            },
        )
    }

    pub fn expect_post_and_return_no_content(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::NoContent { headers: vec![] },
        )
    }

    pub fn expect_post_and_return_not_found(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::Error {
                headers: vec![],
                status: 404,
                body: None,
            },
        )
    }

    pub fn expect_post_and_return_internal_server_error(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::Error {
                headers: vec![],
                status: 500,
                body: None,
            },
        )
    }

    pub fn expect_post_and_return(
        &mut self,
        path: &str,
        headers: Vec<Header>,
        body: Option<String>,
        status: u16,
        response_body: Option<String>,
    ) {
        self.expect_rest_call(
            self.create_post_expectation(path, headers, body),
            Response::Error {
                headers: vec![],
                status,
                body: response_body,
            },
        )
    }
}

#[derive(Debug)]
pub struct SimpleRestClient<TIo: IoStream> {
    scheme: String,
    authority: String,
    io_stream: Option<TIo>,
    sender: Option<SendRequest<String>>,
    default_headers: Vec<Header>,
    logger: Arc<dyn Logger>,
}

impl<TIo: IoStream + 'static> SimpleRestClient<TIo> {
    pub fn new(
        scheme: &str,
        authority: &str,
        io_stream: TIo,
        default_headers: Vec<Header>,
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
    ) -> Result<hyper::Request<String>, RestClientError> {
        let uri_builder = hyper::http::uri::Builder::new()
            .scheme(self.scheme.as_str())
            .authority(self.authority.as_str());
        let mut request_builder = hyper::Request::builder();
        let request_headers: &Vec<Header>;
        let request_body: &Option<String>;

        match request {
            Request::Delete { path, headers } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builder = request_builder.method("DELETE").uri(uri);
                request_headers = headers;
                request_body = &None;
            }
            Request::Get { path, headers } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builder = request_builder.method("GET").uri(uri);
                request_headers = headers;
                request_body = &None;
            }
            Request::Post {
                path,
                headers,
                body,
            } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builder = request_builder.method("POST").uri(uri);
                request_body = body;
                request_headers = headers;
            }
            Request::Put {
                path,
                headers,
                body,
            } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builder = request_builder.method("PUT").uri(uri);
                request_body = body;
                request_headers = headers;
            }
        }

        for headers in [&self.default_headers, request_headers] {
            for header in headers {
                request_builder = request_builder.header(header.name.clone(), header.value.clone());
            }
        }

        Ok(request_builder.body(request_body.clone().unwrap_or_default())?)
    }

    async fn send_hyper_request(
        &mut self,
        hyper_request: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, hyper::Error> {
        self.sender
            .as_mut()
            .unwrap()
            .send_request(hyper_request)
            .await
    }

    async fn initialize_sender(&mut self) -> Result<(), RestClientError> {
        if self.sender.is_none() {
            let io = self
                .io_stream
                .take()
                .ok_or(RestClientError::IoStreamAlreadyTaken)?;

            let (sender, conn) = hyper::client::conn::http1::handshake::<TIo, String>(io).await?;
            let task_logger = Arc::clone(&self.logger);
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    task_logger.error(&format!("Connection error: {:?}", e));
                }
            });

            self.sender = Some(sender);
        }

        Ok(())
    }

    fn create_response(
        &self,
        status: hyper::StatusCode,
        headers: Vec<Header>,
        body: Option<String>,
    ) -> Result<Response, RestClientError> {
        match status {
            hyper::StatusCode::OK => Ok(Response::Okay { headers, body }),
            hyper::StatusCode::CREATED => Ok(Response::Created { headers, body }),
            hyper::StatusCode::NO_CONTENT => Ok(Response::NoContent { headers }),
            _ => Ok(Response::Error {
                headers,
                status: status.as_u16(),
                body,
            }),
        }
    }

    async fn parse_hyper_response(
        &self,
        hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<(hyper::StatusCode, Vec<Header>, Option<String>), RestClientError> {
        let status = hyper_response.status();
        let headers: Vec<Header> = hyper_response
            .headers()
            .iter()
            .map(|(header_name, header_value)| Header {
                name: header_name.to_string(),
                value: header_value.to_str().unwrap().to_string(),
            })
            .collect();
        let raw_body = self.read_raw_body(hyper_response).await?;
        self.log_response(status.as_u16(), &headers, &raw_body);
        Ok((status, headers, raw_body))
    }

    async fn read_raw_body(
        &self,
        mut hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Option<String>, RestClientError> {
        let mut buffer = String::new();

        while let Some(next) = hyper_response.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                buffer.push_str(core::str::from_utf8(chunk).unwrap());
            }
        }

        if buffer.is_empty() {
            Ok(None)
        } else {
            Ok(Some(buffer))
        }
    }

    fn pretty_headers(&self, headers: &[Header]) -> String {
        headers
            .iter()
            .map(|header| format!("'{}={}'", header.name, header.value))
            .collect::<Vec<String>>()
            .join(", ")
    }

    fn log_request(&self, request: &Request) {
        let verb: &str;
        let request_path: &String;
        let request_headers: &Vec<Header>;
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

        let headers = self.pretty_headers(request_headers);

        let mut result = format!(
            "Performing '{}' on '{}://{}{}', with headers {}",
            verb, self.scheme, self.authority, request_path, headers
        );

        if let Some(body) = request_body {
            result.push_str(&format!(", with body {}", body));
        }

        self.logger.info(&result);
    }

    fn log_response(
        &self,
        status_code: u16,
        response_headers: &[Header],
        response_body: &Option<String>,
    ) {
        let mut result = format!(
            "Received '{}' with headers {}, ",
            status_code,
            self.pretty_headers(response_headers)
        );

        match response_body {
            None => result.push_str("and no body"),
            Some(body) => result.push_str(&format!("with body '{}'", body)),
        }

        self.logger.info(&result);
    }
}

#[async_trait::async_trait]
impl<TIo> RestClient for SimpleRestClient<TIo>
where
    TIo: IoStream + 'static,
{
    async fn execute(&mut self, request: &Request) -> Result<Response, RestClientError> {
        self.log_request(request);
        let hyper_request = self.build_hyper_request(request)?;

        self.initialize_sender().await?;
        let hyper_response = self.send_hyper_request(hyper_request).await?;

        let (status, headers, raw_body) = self.parse_hyper_response(hyper_response).await?;
        self.create_response(status, headers, raw_body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_util::rt::TokioIo;
    use log::MockLogger;
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
            .with(contains(
                "Received '200' with headers 'content-length=0', and no body",
            ))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: vec![],
            })
            .await;

        assert!(result.is_ok());

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

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let _: Result<Response, _> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: vec![],
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
                "Received '201' with headers 'content-length=11', with body 'hello world'",
            ))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: vec![],
            })
            .await;

        assert!(result.is_ok());

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
        logger.expect_info().return_const(());
        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: vec![],
            })
            .await;

        assert!(result.is_ok());

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

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Get {
                path: "/".to_string(),
                headers: vec![],
            })
            .await;

        assert!(result.is_ok());

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

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Delete {
                path: "/".to_string(),
                headers: vec![],
            })
            .await;

        assert!(result.is_ok());

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

        let mut client = SimpleRestClient::new("HTTP", "localhost", io, vec![], Arc::new(logger));

        let result: Result<Response, _> = client
            .execute(&Request::Put {
                path: "/".to_string(),
                headers: vec![],
                body: Some("body".to_string()),
            })
            .await;

        assert!(result.is_ok());

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
            vec![Header::new("foo", "bar")],
            Arc::new(logger),
        );

        let result: Result<Response, _> = client
            .execute(&Request::Post {
                path: "/".to_string(),
                headers: vec![Header::new("baz", "qux")],
                body: Some("body".to_string()),
            })
            .await;

        assert!(result.is_ok());

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

    mod query_string {
        use super::super::*;

        #[test]
        fn should_make_query_string_without_params() {
            let actual = create_path_and_query_string("/foo/bar", HashMap::<&str, &str>::new());
            assert_eq!("/foo/bar", actual);
        }

        #[test]
        fn should_make_query_string_with_params() {
            let actual =
                create_path_and_query_string("/foo/bar", HashMap::from([("bas", "qux & quux")]));
            assert_eq!("/foo/bar?bas=qux%20%26%20quux", actual);
        }
    }
}
