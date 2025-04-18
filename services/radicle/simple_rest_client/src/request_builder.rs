use hyper::Request;
use hyper::http::uri;
use std::error::Error;

pub trait RequestBuilder {
    fn build(
        &self,
        verb: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<Request<String>, Box<dyn Error>>;
}

pub struct LocalhostRequestBuilder;

impl LocalhostRequestBuilder {
    pub fn new() -> LocalhostRequestBuilder {
        Self {}
    }
}

impl RequestBuilder for LocalhostRequestBuilder {
    fn build(
        &self,
        verb: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<Request<String>, Box<dyn Error>> {
        let authority: &str = "localhost";
        let uri = uri::Builder::new()
            .scheme("http")
            .authority(authority)
            .path_and_query(path)
            .build()
            .unwrap();

        Ok(Request::builder()
            .method(verb)
            .uri(uri)
            .header(hyper::header::HOST, authority)
            .body(body.unwrap_or("").into())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_create_a_local_request() {
        let result = LocalhostRequestBuilder {}.build("verb", "/path", Some("body"));

        assert!(result.is_ok());

        let (parts, body) = result.unwrap().into_parts();
        assert_eq!("verb", parts.method);
        assert_eq!("body", body);
        assert_eq!(1, parts.headers.len());

        let (name, value) = parts.headers.iter().next().unwrap();
        assert_eq!(hyper::header::HOST, name.as_str());
        assert_eq!("localhost", value.to_str().unwrap());

        let parts = parts.uri.into_parts();
        assert_eq!("http", parts.scheme.unwrap().as_str());
        assert_eq!("localhost", parts.authority.unwrap().host());
        assert_eq!("/path", parts.path_and_query.unwrap().path());
    }

    #[test]
    fn should_return_error_for_invalid() {
        let result = LocalhostRequestBuilder {}.build("spaces are invalid in URIs", "/path", None);

        assert_eq!(false, result.is_ok());
    }
}
