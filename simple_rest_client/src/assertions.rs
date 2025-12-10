use crate::{Header, Response};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AssertionError {
    #[error("Received unexpected response with status: {status}, {message}")]
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
