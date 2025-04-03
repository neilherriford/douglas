use std::fmt;

use http_body_util::BodyExt;
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::path::Path;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, PartialEq)]
pub enum Verb {
    Get,
    Post,
    Put,
    Other(String),
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Verb::Get => write!(f, "GET"),
            Verb::Post => write!(f, "POST"),
            Verb::Put => write!(f, "PUT"),
            Verb::Other(value) => write!(f, "{}", value.to_uppercase()),
        }
    }
}

pub fn create_request(
    path: String,
    verb: Option<Verb>,
    body: Option<String>,
) -> Result<Request<String>, Box<dyn std::error::Error>> {
    let url = format!("http://localhost{}", &path).parse::<hyper::Uri>()?;
    let authority = url.authority().unwrap().clone();

    let req = Request::builder()
        .method(verb.unwrap_or(Verb::Get).to_string().as_str())
        .uri(url)
        .header(hyper::header::HOST, authority.as_str())
        .body(body.unwrap_or(String::new()))?;

    Ok(req)
}

pub async fn buffer(
    socket_path: String,
    req: Request<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = Path::new(&socket_path);
    let stream = UnixStream::connect(socket_path).await?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::task::spawn(async move {
        if let Err(err) = conn.await {
            println!("Connection failed: {:?}", err);
        }
    });

    let mut res = sender.send_request(req).await?;
    println!("Response status: {}", res.status());

    while let Some(next) = res.frame().await {
        let frame = next?;
        if let Some(chunk) = frame.data_ref() {
            io::stdout().write_all(chunk).await?;
        }
    }

    Ok(())
}
