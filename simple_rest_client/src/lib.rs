pub mod assertions;
pub mod parsers;
pub mod unix_domain_socket;

use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use log::{Level, Outcome, ScopeKind, Span};
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

impl PartialEq for RestClientError {
    fn eq(&self, other: &Self) -> bool {
        match self {
            RestClientError::Stream(left) => {
                if let RestClientError::Stream(right) = other {
                    left.to_string() == right.to_string()
                } else {
                    false
                }
            }
            RestClientError::Client(left) => {
                if let RestClientError::Client(right) = other {
                    left.to_string() == right.to_string()
                } else {
                    false
                }
            }
            _ => other == self,
        }
    }
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

impl Request {
    pub fn to_short_description(&self) -> String {
        match self {
            Request::Delete { path, .. } => format!("DELETE {path}"),
            Request::Get { path, .. } => format!("GET {path}"),
            Request::Post { path, .. } => format!("POST {path}"),
            Request::Put { path, .. } => format!("PUT {path}"),
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
    async fn execute(
        &mut self,
        span: &Span,

        request: &Request,
    ) -> Result<Response, RestClientError>;
}

/*
 * hyper's HTTP/1.1 connection future resolves to Err when the server
 * closes the connection (e.g. after a keep-alive timeout or a single
 * request connection)
 */
#[derive(Debug, PartialEq, Eq)]
pub enum ServerClosedConnections {
    Ignore,
    TreatAsError,
}

pub struct SimpleRestClient<TIo: IoStream> {
    scheme: String,
    authority: String,
    io_stream: Option<TIo>,
    sender: Option<SendRequest<String>>,
    default_headers: Vec<Header>,
    server_closed_connections: ServerClosedConnections,
}

impl<TIo: IoStream + 'static> SimpleRestClient<TIo> {
    pub fn new(
        scheme: &str,
        authority: &str,
        io_stream: TIo,
        default_headers: Vec<Header>,
        server_closed_connections: ServerClosedConnections,
    ) -> Self {
        Self {
            scheme: scheme.into(),
            authority: authority.into(),
            io_stream: Some(io_stream),
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

            let io = self
                .io_stream
                .take()
                .ok_or(RestClientError::IoStreamAlreadyTaken)?;

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

    async fn parse_hyper_response(
        &self,
        span: &Span,
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
        let hyper_request = self.build_hyper_request(request)?;

        self.initialize_sender(guard.span()).await?;
        let hyper_response = self.send_hyper_request(hyper_request).await?;

        let (status, headers, raw_body) = self
            .parse_hyper_response(guard.span(), hyper_response)
            .await?;
        guard.finish(self.create_response(status, headers, raw_body))
    }
}
