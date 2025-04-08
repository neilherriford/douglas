use hyper::Request;
use hyper::http::uri;
use std::error::Error;

pub(crate) trait RequestBuilder {
    fn build(
        &self,
        verb: String,
        path: String,
        body: Option<String>,
    ) -> Result<Request<String>, Box<dyn Error>>;
}

pub(crate) struct LocalhostRequestBuilder;

impl RequestBuilder for LocalhostRequestBuilder {
    fn build(
        &self,
        verb: String,
        path: String,
        body: Option<String>,
    ) -> Result<Request<String>, Box<dyn Error>> {
        let authority: &str = "localhost";
        let uri = uri::Builder::new()
            .scheme("http")
            .authority(authority)
            .path_and_query(path)
            .build()
            .unwrap();

        Ok(Request::builder()
            .method(verb.as_str())
            .uri(uri)
            .header(hyper::header::HOST, authority)
            .body(body.unwrap_or(String::new()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_create_a_local_request() {
        let result = LocalhostRequestBuilder {}.build(
            String::from("verb"),
            String::from("/path"),
            Some(String::from("body")),
        );

        assert!(result.is_ok());

        let (parts, body) = result.unwrap().into_parts();
        assert_eq!("verb", parts.method);
        assert_eq!("body", body);
        assert_eq!(1, parts.headers.len());

        // The header is needed for local domain sockets
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
        let result = LocalhostRequestBuilder {}.build(
            String::from("spaces are invalid in URIs"),
            String::from("/path"),
            None,
        );

        assert_eq!(false, result.is_ok());
    }
}
