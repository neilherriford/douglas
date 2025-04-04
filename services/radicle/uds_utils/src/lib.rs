use http_body_util::BodyExt;
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::Path;
use tokio::net::UnixStream;

pub enum Response {
    Okay(Option<String>),
    Created(Option<String>),
    NoContent,
    Error { code: u16, message: String },
}

pub struct Client {
    socket_file_path: String,
}

impl Client {
    pub fn new(socket_file_path: String) -> Client {
        Client { socket_file_path }
    }

    pub async fn get(&self, path: String) -> Result<Response, Box<dyn std::error::Error>> {
        let req = Client::create_request(String::from("GET"), path, Some(String::new())).unwrap();
        self.preform_request(req).await
    }

    fn create_request(
        verb: String,
        path: String,
        body: Option<String>,
    ) -> Result<Request<String>, Box<dyn std::error::Error>> {
        let url = format!("http://localhost{}", &path).parse::<hyper::Uri>()?;
        let authority = url.authority().unwrap().clone();

        Ok(Request::builder()
            .method(verb.as_str())
            .uri(url)
            .header(hyper::header::HOST, authority.as_str())
            .body(body.unwrap_or(String::new()))?)
    }

    async fn preform_request(
        &self,
        req: Request<String>,
    ) -> Result<Response, Box<dyn std::error::Error>> {
        let io = self.create_unix_stream_io().await?;
        let mut sender = Client::create_sender(io).await?;
        let res = sender.send_request(req).await?;

        let status = res.status();
        let body = Client::read_body(res).await?;

        Ok(Client::create_response(status, body))
    }

    async fn create_sender(
        io: TokioIo<UnixStream>,
    ) -> Result<hyper::client::conn::http1::SendRequest<String>, Box<dyn std::error::Error>> {
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;

        tokio::task::spawn(async move {
            if let Err(err) = conn.await {
                println!("Connection failed: {:?}", err);
            }
        });

        Ok(sender)
    }

    async fn create_unix_stream_io(
        &self,
    ) -> Result<TokioIo<UnixStream>, Box<dyn std::error::Error>> {
        let socket_path = Path::new(&self.socket_file_path);
        let stream = UnixStream::connect(socket_path).await?;
        Ok(TokioIo::new(stream))
    }

    async fn read_body(
        mut res: hyper::Response<hyper::body::Incoming>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut body = String::new();

        while let Some(next) = res.frame().await {
            let frame = next?;
            if let Some(chunk) = frame.data_ref() {
                body.push_str(String::from_utf8(chunk.to_vec()).unwrap().as_str());
            }
        }

        Ok(body)
    }

    fn create_response(status: StatusCode, body: String) -> Response {
        match status {
            StatusCode::OK => Response::Okay(if body.len() == 0 { None } else { Some(body) }),
            StatusCode::CREATED => {
                Response::Created(if body.len() == 0 { None } else { Some(body) })
            }
            StatusCode::NO_CONTENT => Response::NoContent,
            status => Response::Error {
                code: status.as_u16(),
                message: body,
            },
        }
    }
}
