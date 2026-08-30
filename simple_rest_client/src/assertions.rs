use crate::{Header, Response};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AssertionError {
    #[error("Received unexpected response with status: {status}, {message}: {body:?}")]
    UnexpectedResponseError {
        status: u16,
        body: Option<String>,
        message: String,
    },
    #[error("Expected response to have a body")]
    MissingBody,

    #[error("Not found")]
    NotFoundError,
}

pub fn assert_okay_with_body(response: Response) -> Result<String, AssertionError> {
    let (_, body) = assert_okay(response)?;

    if let Some(body) = body {
        Ok(body)
    } else {
        Err(AssertionError::MissingBody)
    }
}

pub fn assert_okay(response: Response) -> Result<(Vec<Header>, Option<String>), AssertionError> {
    match response {
        Response::Okay { headers, body } => Ok((headers, body)),
        Response::Created { body, .. } => Err(AssertionError::UnexpectedResponseError {
            status: 201,
            body,
            message: "expected OK, but recieved CREATED".to_string(),
        }),
        Response::NoContent { .. } => Err(AssertionError::UnexpectedResponseError {
            status: 204,
            body: None,
            message: "expected OK, but recieved NO CONTENT".to_string(),
        }),
        Response::Error { status: 404, .. } => Err(AssertionError::NotFoundError),
        Response::Error { status, body, .. } => Err(AssertionError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}

pub fn assert_created_with_body(response: Response) -> Result<String, AssertionError> {
    let (_, body) = assert_created(response)?;

    if let Some(body) = body {
        Ok(body)
    } else {
        Err(AssertionError::MissingBody)
    }
}

pub fn assert_created(response: Response) -> Result<(Vec<Header>, Option<String>), AssertionError> {
    match response {
        Response::Okay { headers: _, body } => Err(AssertionError::UnexpectedResponseError {
            status: 200,
            body,
            message: "expected CREATED, but recieved OK".to_string(),
        }),
        Response::Created { headers, body } => Ok((headers, body)),
        Response::NoContent { .. } => Err(AssertionError::UnexpectedResponseError {
            status: 204,
            body: None,
            message: "expected CREATED, but recieved NO CONTENT".to_string(),
        }),
        Response::Error { status: 404, .. } => Err(AssertionError::NotFoundError),
        Response::Error { status, body, .. } => Err(AssertionError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}

pub fn assert_no_content(response: Response) -> Result<Vec<Header>, AssertionError> {
    match response {
        Response::Okay { body, .. } => Err(AssertionError::UnexpectedResponseError {
            status: 200,
            body,
            message: "expected NO CONTENT, but recieved OK".to_string(),
        }),
        Response::Created { body, .. } => Err(AssertionError::UnexpectedResponseError {
            status: 201,
            body,
            message: "expected NO CONTENT, but recieved CREATED".to_string(),
        }),
        Response::NoContent { headers } => Ok(headers),
        Response::Error { status: 404, .. } => Err(AssertionError::NotFoundError),
        Response::Error { status, body, .. } => Err(AssertionError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}

pub fn assert_okay_or_no_content(response: Response) -> Result<Vec<Header>, AssertionError> {
    match response {
        Response::Okay { headers, .. } | Response::NoContent { headers } => Ok(headers),
        Response::Created { body, .. } => Err(AssertionError::UnexpectedResponseError {
            status: 201,
            body,
            message: "expected OK or NO CONTENT, but recieved CREATED".to_string(),
        }),
        Response::Error { status: 404, .. } => Err(AssertionError::NotFoundError),
        Response::Error { status, body, .. } => Err(AssertionError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_response(status: u16, body: Option<&str>) -> Response {
        Response::Error {
            headers: Vec::new(),
            status,
            body: body.map(str::to_string),
        }
    }

    #[test]
    fn test_assert_okay_should_pass_through_a_body() {
        let body = assert_okay_with_body(Response::Okay {
            headers: Vec::new(),
            body: Some("hello".to_string()),
        })
        .expect("should pass through");

        assert_eq!(body, "hello");
    }

    #[test]
    fn test_assert_okay_with_body_should_error_on_a_missing_body() {
        let result = assert_okay_with_body(Response::Okay {
            headers: Vec::new(),
            body: None,
        });

        assert!(matches!(result, Err(AssertionError::MissingBody)));
    }

    #[test]
    fn test_assert_okay_should_treat_404_as_not_found() {
        let result = assert_okay(error_response(404, None));

        assert!(matches!(result, Err(AssertionError::NotFoundError)));
    }

    #[test]
    fn test_assert_okay_should_surface_the_response_body_in_its_display() {
        let result = assert_okay(error_response(400, Some(r#"{"message":"bad request"}"#)));

        let err = result.expect_err("should be an error");
        assert!(err.to_string().contains("bad request"));
        assert!(err.to_string().contains("400"));
    }

    #[test]
    fn test_assert_created_should_pass_through_a_body() {
        let body = assert_created_with_body(Response::Created {
            headers: Vec::new(),
            body: Some("hello".to_string()),
        })
        .expect("should pass through");

        assert_eq!(body, "hello");
    }

    #[test]
    fn test_assert_created_should_treat_404_as_not_found() {
        let result = assert_created(error_response(404, None));

        assert!(matches!(result, Err(AssertionError::NotFoundError)));
    }

    #[test]
    fn test_assert_created_should_reject_an_okay_response() {
        let result = assert_created(Response::Okay {
            headers: Vec::new(),
            body: None,
        });

        assert!(matches!(
            result,
            Err(AssertionError::UnexpectedResponseError { status: 200, .. })
        ));
    }

    #[test]
    fn test_assert_no_content_should_pass_through_headers() {
        let headers = assert_no_content(Response::NoContent {
            headers: vec![Header::new("x-test", "1")],
        })
        .expect("should pass through");

        assert_eq!(headers, vec![Header::new("x-test", "1")]);
    }

    #[test]
    fn test_assert_no_content_should_treat_404_as_not_found() {
        let result = assert_no_content(error_response(404, None));

        assert!(matches!(result, Err(AssertionError::NotFoundError)));
    }

    #[test]
    fn test_assert_no_content_should_surface_the_response_body_in_its_display() {
        let result = assert_no_content(error_response(400, Some("malformed request")));

        let err = result.expect_err("should be an error");
        assert!(err.to_string().contains("malformed request"));
    }

    #[test]
    fn test_assert_okay_or_no_content_should_pass_through_an_okay_response() {
        let headers = assert_okay_or_no_content(Response::Okay {
            headers: vec![Header::new("x-test", "1")],
            body: Some("{}".to_string()),
        })
        .expect("should pass through");

        assert_eq!(headers, vec![Header::new("x-test", "1")]);
    }

    #[test]
    fn test_assert_okay_or_no_content_should_pass_through_a_no_content_response() {
        let headers = assert_okay_or_no_content(Response::NoContent {
            headers: vec![Header::new("x-test", "1")],
        })
        .expect("should pass through");

        assert_eq!(headers, vec![Header::new("x-test", "1")]);
    }

    #[test]
    fn test_assert_okay_or_no_content_should_reject_a_created_response() {
        let result = assert_okay_or_no_content(Response::Created {
            headers: Vec::new(),
            body: None,
        });

        assert!(matches!(
            result,
            Err(AssertionError::UnexpectedResponseError { status: 201, .. })
        ));
    }

    #[test]
    fn test_assert_okay_or_no_content_should_treat_404_as_not_found() {
        let result = assert_okay_or_no_content(error_response(404, None));

        assert!(matches!(result, Err(AssertionError::NotFoundError)));
    }
}
