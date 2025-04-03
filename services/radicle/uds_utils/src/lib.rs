use http_body_util::BodyExt;
use hyper::Request;
use hyper_util::rt::TokioIo;
use std::path::Path;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct Client {
    socket_file_path: String,
}

impl Client {
    pub fn new(socket_file_path: String) -> Client {
        Client { socket_file_path }
    }

    pub async fn get(&self, path: String) -> Result<(), Box<dyn std::error::Error>> {
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        let socket_path = Path::new(&self.socket_file_path);
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
}
