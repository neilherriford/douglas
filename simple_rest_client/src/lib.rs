pub mod assertions;
pub mod parsers;
pub mod tcp_socket;
pub mod tls_socket;
pub mod unix_domain_socket;

use bytes::{Buf, Bytes};
use http_body::Body as HttpBody;
use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use log::{Level, Outcome, ScopeKind, Span};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use thiserror::Error;
use tokio::io::{AsyncRead, ReadBuf};

#[derive(Error, Debug)]
pub enum RestClientError {
    #[error("Stream error: {0}")]
    Stream(#[from] hyper::http::Error),
    #[error("Client error: {0}")]
    Client(#[from] hyper::Error),
    #[error("Connection error: {0}")]
    ConnectionError(std::io::Error),
    #[error("General error: {0}")]
    General(String),
}

impl PartialEq for RestClientError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RestClientError::Stream(left), RestClientError::Stream(right)) => {
                left.to_string() == right.to_string()
            }
            (RestClientError::Client(left), RestClientError::Client(right)) => {
                left.to_string() == right.to_string()
            }
            (RestClientError::ConnectionError(left), RestClientError::ConnectionError(right)) => {
                left.to_string() == right.to_string()
            }
            (RestClientError::General(left), RestClientError::General(right)) => left == right,
            _ => false,
        }
    }
}

#[async_trait::async_trait]
pub trait Reconnect<TIo>: Send + Sync {
    async fn connect(&self) -> std::io::Result<TIo>;
}

#[derive(Debug, PartialEq, Clone)]
pub enum Request {
    Delete {
        path: String,
        headers: Vec<Header>,
    },
    Get {
        path: String,
        headers: Vec<Header>,
    },
    Head {
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

impl Request {
    pub fn to_short_description(&self) -> String {
        match self {
            Request::Delete { path, .. } => format!("DELETE {path}"),
            Request::Get { path, .. } => format!("GET {path}"),
            Request::Head { path, .. } => format!("HEAD {path}"),
            Request::Post { path, .. } => format!("POST {path}"),
            Request::Put { path, .. } => format!("PUT {path}"),
        }
    }

    pub fn headers(&self) -> Vec<Header> {
        match self {
            Request::Delete { headers, .. } => headers,
            Request::Get { headers, .. } => headers,
            Request::Head { headers, .. } => headers,
            Request::Post { headers, .. } => headers,
            Request::Put { headers, .. } => headers,
        }
        .to_vec()
    }

    pub fn path(&self) -> String {
        match self {
            Request::Delete { path, .. } => path.clone(),
            Request::Get { path, .. } => path.clone(),
            Request::Head { path, .. } => path.clone(),
            Request::Post { path, .. } => path.clone(),
            Request::Put { path, .. } => path.clone(),
        }
    }
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

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Response::Okay { .. } => f.write_str("Okay"),
            Response::Created { .. } => f.write_str("Created"),
            Response::NoContent { .. } => f.write_str("No content"),
            Response::Error { status, .. } => f.write_str(&format!("Error: {status}")),
        }
    }
}

pub enum StreamedResponse {
    Okay {
        headers: Vec<Header>,
        body: Box<dyn AsyncRead + Send + Unpin>,
    },
    Created {
        headers: Vec<Header>,
        body: Box<dyn AsyncRead + Send + Unpin>,
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

struct BodyReader {
    body: hyper::body::Incoming,
    buffer: Bytes,
}

impl BodyReader {
    fn new(body: hyper::body::Incoming) -> Self {
        Self {
            body,
            buffer: Bytes::new(),
        }
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        loop {
            if !this.buffer.is_empty() {
                let amount = std::cmp::min(buf.remaining(), this.buffer.len());
                buf.put_slice(&this.buffer[..amount]);
                this.buffer.advance(amount);
                return Poll::Ready(Ok(()));
            }

            match Pin::new(&mut this.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => this.buffer = data,
                    Err(_) => continue,
                },
                Poll::Ready(Some(Err(err))) => {
                    return Poll::Ready(Err(std::io::Error::other(err)));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn content_type_json() -> Header {
        Header::new(
            hyper::header::CONTENT_TYPE.as_str(),
            mime::APPLICATION_JSON.as_ref(),
        )
    }

    pub fn authorization_bearer(token: &str) -> Header {
        Header::new(
            hyper::header::AUTHORIZATION.as_str(),
            &format!("Bearer {token}"),
        )
    }

    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

pub mod header_predicates {
    use crate::Header;

    pub fn named(name: &'static str) -> impl Fn(&&Header) -> bool {
        move |header| header.name.eq_ignore_ascii_case(name)
    }

    pub fn is_content_type() -> impl Fn(&&Header) -> bool {
        named(hyper::header::CONTENT_TYPE.as_str())
    }

    pub fn is_authorization() -> impl Fn(&&Header) -> bool {
        named(hyper::header::AUTHORIZATION.as_str())
    }

    pub fn is_content_length() -> impl Fn(&&Header) -> bool {
        named(hyper::header::CONTENT_LENGTH.as_str())
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
    async fn execute(
        &mut self,
        span: &Span,
        request: &Request,
    ) -> Result<Response, RestClientError>;

    async fn execute_streaming(
        &mut self,
        span: &Span,
        request: &Request,
    ) -> Result<StreamedResponse, RestClientError>;
}

/*
 * hyper's HTTP/1.1 connection future resolves to Err when the server
 * closes the connection (e.g. after a keep-alive timeout or a single
 * request connection)
 */
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ServerClosedConnections {
    Ignore,
    TreatAsError,
}

pub struct SimpleRestClient<TIo: IoStream> {
    scheme: String,
    authority: String,
    io_stream: Option<TIo>,
    reconnect: Arc<dyn Reconnect<TIo>>,
    sender: Option<SendRequest<String>>,
    default_headers: Vec<Header>,
    server_closed_connections: ServerClosedConnections,
}

impl<TIo: IoStream + 'static> SimpleRestClient<TIo> {
    pub fn new(
        scheme: &str,
        authority: &str,
        io_stream: TIo,
        reconnect: Arc<dyn Reconnect<TIo>>,
        default_headers: Vec<Header>,
        server_closed_connections: ServerClosedConnections,
    ) -> Self {
        Self {
            scheme: scheme.into(),
            authority: authority.into(),
            io_stream: Some(io_stream),
            reconnect,
            sender: None,
            default_headers,
            server_closed_connections,
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
            Request::Head { path, headers } => {
                let uri = uri_builder.path_and_query(path).build()?;
                request_builder = request_builder.method("HEAD").uri(uri);
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

    async fn initialize_sender(&mut self, span: &Span) -> Result<(), RestClientError> {
        if self.sender.is_none() {
            let guard = span
                .create_child("Initialize sender", ScopeKind::Task)
                .start_guard();

            let io = match self.io_stream.take() {
                Some(io) => io,
                None => self
                    .reconnect
                    .connect()
                    .await
                    .map_err(RestClientError::ConnectionError)?,
            };

            let (sender, conn) = hyper::client::conn::http1::handshake::<TIo, String>(io).await?;

            guard.finish_with_outcome(Outcome::Ok);

            /*
             * Use a weak reference so this background task doesn't keep
             * the reporter alive. If the reporter is already gone when
             * the connection closes, the error is silently dropped.
             * It's the responsibility of the caller to clean that up
             */
            let conn_reporter = Arc::downgrade(&span.reporter);
            let conn_scope_id = span.id;
            let report_closed_connections =
                self.server_closed_connections == ServerClosedConnections::TreatAsError;
            tokio::spawn(async move {
                if let Err(e) = conn.await
                    && (report_closed_connections || (!e.is_closed() && !e.is_incomplete_message()))
                    && let Some(reporter) = conn_reporter.upgrade()
                {
                    reporter.emit(log::Event::new(
                        conn_scope_id,
                        log::EventKind::Message {
                            level: Level::Warn,
                            text: format!("Connection error: {e:?}"),
                        },
                    ));
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

    fn gather_headers(hyper_response: &hyper::Response<hyper::body::Incoming>) -> Vec<Header> {
        let headers: Vec<Header> = hyper_response
            .headers()
            .iter()
            .map(|(header_name, header_value)| Header {
                name: header_name.to_string(),
                value: header_value.to_str().unwrap().to_string(),
            })
            .collect();
        headers
    }

    async fn parse_hyper_response(
        &self,
        span: &Span,
        hyper_response: hyper::Response<hyper::body::Incoming>,
    ) -> Result<(hyper::StatusCode, Vec<Header>, Option<String>), RestClientError> {
        let status = hyper_response.status();
        let headers = Self::gather_headers(&hyper_response);
        let raw_body = self.read_raw_body(hyper_response).await?;
        self.log_response(span, status.as_u16(), &headers, &raw_body);
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

    fn log_request(&self, span: &Span, request: &Request) {
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
            Request::Head { path, headers } => {
                verb = "HEAD";
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

        span.message(Level::Info, &result);
    }

    async fn send_with_reconnect(
        &mut self,
        span: &Span,
        request: &Request,
    ) -> Result<hyper::Response<hyper::body::Incoming>, RestClientError> {
        self.initialize_sender(span).await?;
        let hyper_request = self.build_hyper_request(request)?;

        match self.send_hyper_request(hyper_request).await {
            Ok(response) => Ok(response),
            Err(_) => {
                span.message(
                    Level::Warn,
                    "Connection appears dead, reconnecting and retrying once",
                );
                self.sender = None;
                self.initialize_sender(span).await?;
                let retry_request = self.build_hyper_request(request)?;
                Ok(self.send_hyper_request(retry_request).await?)
            }
        }
    }

    fn log_response(
        &self,
        span: &Span,
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

        span.message(Level::Info, &result);
    }
}

#[async_trait::async_trait]
impl<TIo> RestClient for SimpleRestClient<TIo>
where
    TIo: IoStream + 'static,
{
    async fn execute(
        &mut self,
        span: &Span,
        request: &Request,
    ) -> Result<Response, RestClientError> {
        let guard = span
            .create_child(
                &format!("Executing request '{}'", request.to_short_description()),
                ScopeKind::Task,
            )
            .start_guard();

        self.log_request(guard.span(), request);
        let hyper_response = self.send_with_reconnect(guard.span(), request).await?;

        let (status, headers, raw_body) = self
            .parse_hyper_response(guard.span(), hyper_response)
            .await?;
        guard.finish(self.create_response(status, headers, raw_body))
    }

    async fn execute_streaming(
        &mut self,
        span: &Span,
        request: &Request,
    ) -> Result<StreamedResponse, RestClientError> {
        let guard = span
            .create_child(
                &format!(
                    "Executing streaming request '{}'",
                    request.to_short_description()
                ),
                ScopeKind::Task,
            )
            .start_guard();

        self.log_request(guard.span(), request);
        let hyper_response = self.send_with_reconnect(guard.span(), request).await?;
        let headers = Self::gather_headers(&hyper_response);
        let status = hyper_response.status();

        if status == hyper::StatusCode::NO_CONTENT {
            return Ok(StreamedResponse::NoContent { headers });
        }

        if status.is_success() {
            let body = Box::new(BodyReader::new(hyper_response.into_body()))
                as Box<dyn AsyncRead + Send + Unpin>;
            Ok(if status == hyper::StatusCode::CREATED {
                StreamedResponse::Created { headers, body }
            } else {
                StreamedResponse::Okay { headers, body }
            })
        } else {
            let body = self.read_raw_body(hyper_response).await?;
            Ok(StreamedResponse::Error {
                headers,
                status: status.as_u16(),
                body,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str) -> Request {
        Request::Get {
            path: path.to_string(),
            headers: vec![],
        }
    }

    mod create_path_and_query_string {
        use super::*;

        #[test]
        fn test_create_path_and_query_string_should_return_bare_path_when_no_parameters() {
            let result = create_path_and_query_string("/foo", HashMap::new());

            assert_eq!(result, "/foo");
        }

        #[test]
        fn test_create_path_and_query_string_should_append_encoded_parameters() {
            let mut parameters = HashMap::new();
            parameters.insert("a b", "c&d");

            let result = create_path_and_query_string("/foo", parameters);

            assert_eq!(result, "/foo?a%20b=c%26d");
        }
    }

    mod header {
        use super::*;

        #[test]
        fn test_content_type_json_should_set_the_content_type_header() {
            let header = Header::content_type_json();

            assert_eq!(header.name, "content-type");
            assert_eq!(header.value, "application/json");
        }

        #[test]
        fn test_authorization_bearer_should_format_the_token() {
            let header = Header::authorization_bearer("abc123");

            assert_eq!(header.name, "authorization");
            assert_eq!(header.value, "Bearer abc123");
        }
    }

    mod request {
        use super::*;

        #[test]
        fn test_to_short_description_should_include_verb_and_path() {
            let cases = vec![
                (
                    Request::Delete {
                        path: "/foo".to_string(),
                        headers: vec![],
                    },
                    "DELETE /foo",
                ),
                (get("/foo"), "GET /foo"),
                (
                    Request::Head {
                        path: "/foo".to_string(),
                        headers: vec![],
                    },
                    "HEAD /foo",
                ),
                (
                    Request::Post {
                        path: "/foo".to_string(),
                        headers: vec![],
                        body: None,
                    },
                    "POST /foo",
                ),
                (
                    Request::Put {
                        path: "/foo".to_string(),
                        headers: vec![],
                        body: None,
                    },
                    "PUT /foo",
                ),
            ];

            for (request, expected) in cases {
                assert_eq!(request.to_short_description(), expected);
            }
        }

        #[test]
        fn test_headers_should_return_the_requests_headers() {
            let request = Request::Post {
                path: "/foo".to_string(),
                headers: vec![Header::new("x-test", "1")],
                body: None,
            };

            assert_eq!(request.headers(), vec![Header::new("x-test", "1")]);
        }

        #[test]
        fn test_path_should_return_the_requests_path() {
            let request = Request::Put {
                path: "/foo".to_string(),
                headers: vec![],
                body: None,
            };

            assert_eq!(request.path(), "/foo");
        }
    }

    mod rest_client_error {
        use super::*;

        #[test]
        fn test_eq_should_treat_same_general_errors_as_equal() {
            let left = RestClientError::General("foo".to_string());
            let right = RestClientError::General("foo".to_string());

            assert_eq!(left, right);
        }

        #[test]
        fn test_eq_should_treat_same_connection_errors_as_equal() {
            let left = RestClientError::ConnectionError(std::io::Error::other("boom"));
            let right = RestClientError::ConnectionError(std::io::Error::other("boom"));

            assert_eq!(left, right);
        }

        #[test]
        fn test_eq_should_treat_different_variants_as_unequal() {
            let left = RestClientError::General("oops".to_string());
            let right = RestClientError::ConnectionError(std::io::Error::other("boom"));

            assert_ne!(left, right);
        }

        #[test]
        fn test_eq_should_treat_different_general_messages_as_unequal() {
            let left = RestClientError::General("oops".to_string());
            let right = RestClientError::General("uhoh".to_string());

            assert_ne!(left, right);
        }
    }

    mod reconnect_behavior {
        use super::*;
        use hyper::rt::{Read as HyperRead, ReadBufCursor, Write as HyperWrite};
        use hyper_util::rt::TokioIo;
        use log::Event;
        use std::time::Duration;
        use tokio::io::DuplexStream;

        struct NullReporter;

        impl log::Reporter for NullReporter {
            fn emit(&self, _event: Event) {}
        }

        struct TestIoStream {
            stream: TokioIo<DuplexStream>,
        }

        impl HyperRead for TestIoStream {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: ReadBufCursor<'_>,
            ) -> Poll<std::io::Result<()>> {
                Pin::new(&mut self.stream).poll_read(cx, buf)
            }
        }

        impl HyperWrite for TestIoStream {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Pin::new(&mut self.stream).poll_write(cx, buf)
            }

            fn poll_flush(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Pin::new(&mut self.stream).poll_flush(cx)
            }

            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Pin::new(&mut self.stream).poll_shutdown(cx)
            }
        }

        impl IoStream for TestIoStream {}

        struct TestReconnect {
            connect_count: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl Reconnect<TestIoStream> for TestReconnect {
            async fn connect(&self) -> std::io::Result<TestIoStream> {
                self.connect_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (client_end, server_end) = tokio::io::duplex(4096);

                tokio::spawn(async move {
                    let service = hyper::service::service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .header("connection", "close")
                                .body(http_body_util::Full::new(bytes::Bytes::from("ok")))
                                .unwrap(),
                        )
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(server_end), service)
                        .await;
                });

                Ok(TestIoStream {
                    stream: TokioIo::new(client_end),
                })
            }
        }

        #[tokio::test]
        async fn test_execute_should_reconnect_and_retry_after_the_server_closes_a_cached_connection()
         {
            let connect_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let reconnect = Arc::new(TestReconnect {
                connect_count: Arc::clone(&connect_count),
            });
            let first_io = reconnect
                .connect()
                .await
                .expect("initial connect should succeed");

            let mut client = SimpleRestClient::new(
                "http",
                "localhost",
                first_io,
                reconnect,
                vec![],
                ServerClosedConnections::Ignore,
            );

            let span = Span::new(Arc::new(NullReporter), "test", ScopeKind::Group);
            let request = get("/foo");

            let first = client
                .execute(&span, &request)
                .await
                .expect("first request should succeed");
            assert!(matches!(first, Response::Okay { .. }));

            tokio::time::sleep(Duration::from_millis(50)).await;

            let second = client
                .execute(&span, &request)
                .await
                .expect("second request should succeed via reconnect");
            assert!(matches!(second, Response::Okay { .. }));

            assert_eq!(connect_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        }

        struct FlakyFirstConnectionReconnect {
            connect_count: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl Reconnect<TestIoStream> for FlakyFirstConnectionReconnect {
            async fn connect(&self) -> std::io::Result<TestIoStream> {
                let attempt = self
                    .connect_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (client_end, mut server_end) = tokio::io::duplex(4096);

                if attempt == 0 {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1];
                        let _ = tokio::io::AsyncReadExt::read(&mut server_end, &mut buf).await;
                    });
                } else {
                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(|_req| async {
                            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                http_body_util::Full::new(bytes::Bytes::from("ok")),
                            ))
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(server_end), service)
                            .await;
                    });
                }

                Ok(TestIoStream {
                    stream: TokioIo::new(client_end),
                })
            }
        }

        #[tokio::test]
        async fn test_execute_should_retry_when_the_very_first_connection_is_dead_on_arrival() {
            let connect_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let reconnect = Arc::new(FlakyFirstConnectionReconnect {
                connect_count: Arc::clone(&connect_count),
            });
            let first_io = reconnect
                .connect()
                .await
                .expect("initial connect should succeed");

            let mut client = SimpleRestClient::new(
                "http",
                "localhost",
                first_io,
                reconnect,
                vec![],
                ServerClosedConnections::Ignore,
            );

            let span = Span::new(Arc::new(NullReporter), "test", ScopeKind::Group);
            let request = get("/foo");

            let response = client
                .execute(&span, &request)
                .await
                .expect("should recover via a retry on the very first request");
            assert!(matches!(response, Response::Okay { .. }));

            assert_eq!(connect_count.load(std::sync::atomic::Ordering::SeqCst), 2);
        }
    }
}
