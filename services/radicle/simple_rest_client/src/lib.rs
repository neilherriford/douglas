pub mod log;
pub mod unix_domain_socket;

use crate::log::Logger;
use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RestClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] hyper::http::Error),

    #[error("HTTP client error: {0}")]
    HttpClient(#[from] hyper::Error),

    #[error("Parser error: {0}")]
    Parser(String),

    #[error("IO stream already taken")]
    IoStreamAlreadyTaken,
}

#[derive(Debug, PartialEq)]
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

#[derive(Debug, PartialEq)]
pub enum Response<T> {
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

#[derive(Debug, PartialEq)]
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

pub fn create_path_and_query_string(path: &str, parameters: HashMap<&str, &str>) -> String {
    if parameters.len() == 0 {
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
pub trait RestClient<T: Send> {
    async fn execute(&mut self, request: &Request) -> Result<Response<T>, RestClientError>;
}

pub trait Parser<TIn, TOut>: Send + Sync + std::fmt::Debug {
    type ParseError: std::error::Error + Send + Sync + 'static;
    fn parse(&self, input: TIn) -> Result<TOut, Self::ParseError>;
}

#[derive(Debug)]
pub struct SimpleRestClient<
    TIo: IoStream,
    TResponseBody: Send + Sync,
    TParser: Parser<String, TResponseBody>,
> {
    parser: TParser,
    scheme: String,
    authority: String,
    io_stream: Option<TIo>,
    sender: Option<SendRequest<String>>,
    default_headers: Option<Vec<Header>>,
    logger: Arc<dyn Logger>,
    _marker: PhantomData<TResponseBody>,
}

impl<TIo: IoStream + 'static, TResponseBody: Send + Sync, TParser: Parser<String, TResponseBody>>
    SimpleRestClient<TIo, TResponseBody, TParser>
{
    pub fn new(
        scheme: &str,
        authority: &str,
        io_stream: TIo,
        default_headers: Option<Vec<Header>>,
        logger: Arc<dyn Logger>,
        parser: TParser,
    ) -> Self {
        Self {
            scheme: scheme.into(),
            authority: authority.into(),
            io_stream: Some(io_stream),
            sender: None,
            default_headers,
            logger,
            parser: parser,
            _marker: PhantomData,
        }
    }

    fn build_hyper_request(
        &self,
        request: &Request,
    ) -> Result<hyper::Request<String>, RestClientError> {
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

        Ok(request_builer.body(request_body.clone().unwrap_or(String::new()))?)
    }

    async fn send_request(
        &mut self,
        request: hyper::Request<String>,
    ) -> Result<Response<TResponseBody>, RestClientError> {
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

        let response = self.sender.as_mut().unwrap().send_request(request).await?;
        Ok(self.build_response_from_hyper_response(response).await?)
    }

    async fn build_response_from_hyper_response(
        &self,
        hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Response<TResponseBody>, RestClientError> {
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

        let body = match raw_body {
            None => None,
            Some(text) => match self.parser.parse(text) {
                Ok(parsed) => Some(parsed),
                Err(err) => return Err(RestClientError::Parser(err.to_string())),
            },
        };

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

    async fn read_raw_body(
        &self,
        mut hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<Option<String>, RestClientError> {
        let mut buffer = String::new();

        while let Some(next) = hyper_response.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                buffer.push_str(&String::from_utf8(chunk.to_vec()).unwrap());
            }
        }

        if buffer.len() == 0 {
            Ok(None)
        } else {
            Ok(Some(buffer))
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

    fn log_response(
        &self,
        status_code: u16,
        response_headers: &Vec<Header>,
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
impl<TResponseBody, TIo, TParser> RestClient<TResponseBody>
    for SimpleRestClient<TIo, TResponseBody, TParser>
where
    TResponseBody: Send + Sync,
    TParser: Parser<String, TResponseBody>,
    TIo: IoStream + 'static,
{
    async fn execute(
        &mut self,
        request: &Request,
    ) -> Result<Response<TResponseBody>, RestClientError> {
        self.log_request(request);
        let hyper_request = self.build_hyper_request(&request)?;
        let response: Response<TResponseBody> = self.send_request(hyper_request).await?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::MockLogger;
    use hyper_util::rt::TokioIo;
    use mockall::predicate::str::contains;

    use thiserror::Error;
    use tokio_test::io::Mock;

    impl IoStream for TokioIo<Mock> {}

    #[derive(Debug)]
    struct PassthroughParser {}

    #[derive(Error, Debug)]
    enum PassthroughParserError {}

    impl<T> Parser<T, T> for PassthroughParser {
        type ParseError = PassthroughParserError;

        fn parse(&self, input: T) -> Result<T, Self::ParseError> {
            Ok(input)
        }
    }

    impl PassthroughParser {
        pub fn new() -> Self {
            Self {}
        }
    }

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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let _: Result<Response<String>, _> = client
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
                "Received '201' with headers 'content-length=11', with body 'hello world'",
            ))
            .times(1)
            .return_const(());

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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

        let mut client = SimpleRestClient::new(
            "HTTP",
            "localhost",
            io,
            None,
            Arc::new(logger),
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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
            PassthroughParser::new(),
        );

        let result: Result<Response<String>, _> = client
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
