pub(crate) mod container;
pub(crate) mod image;
pub(crate) mod json_parser;
pub(crate) mod network;
pub(crate) mod ping;

use crate::DockerError;
pub use image::ImageCommand;
use serde_json::Value as Json;
use simple_rest_client::{Header, Response};

fn assert_non_empty_string_argument(
    argument_name: &str,
    argument: &str,
) -> Result<(), DockerError> {
    if argument.is_empty() {
        Err(DockerError::InvalidArgumentError {
            name: argument_name.to_string(),
            given: argument.to_string(),
            message: "Cannot be blank".to_string(),
        })
    } else {
        Ok(())
    }
}

fn assert_no_docker_errors(responses: Vec<Json>) -> Result<(), DockerError> {
    for response in responses {
        if let Some(message) = response.get("error") {
            let msg = match message.as_str() {
                Some(text) => text.to_string(),
                None => message.to_string(),
            };

            return Err(DockerError::ApiError(msg));
        }
    }

    Ok(())
}

fn assert_okay_with_body(response: Response) -> Result<String, DockerError> {
    let (_, body) = assert_okay(response)?;

    if let Some(body) = body {
        Ok(body)
    } else {
        Err(DockerError::ParseError {
            line: 0,
            column: 0,
            message: "Received empty response".to_string(),
        })
    }
}

fn assert_okay(response: Response) -> Result<(Vec<Header>, Option<String>), DockerError> {
    match response {
        Response::Okay { headers, body } => Ok((headers, body)),
        Response::Created { body, .. } => Err(DockerError::UnexpectedResponseError {
            status: 201,
            body,
            message: "expected OK, but recieved CREATED".to_string(),
        }),
        Response::NoContent { .. } => Err(DockerError::UnexpectedResponseError {
            status: 204,
            body: None,
            message: "expected OK, but recieved NO CONTENT".to_string(),
        }),
        Response::Error { status: 404, .. } => Err(DockerError::NotFoundError),
        Response::Error { status, body, .. } => Err(DockerError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}

fn assert_created_with_body(response: Response) -> Result<String, DockerError> {
    let (_, body) = assert_created(response)?;

    if let Some(body) = body {
        Ok(body)
    } else {
        Err(DockerError::ParseError {
            line: 0,
            column: 0,
            message: "Received empty response".to_string(),
        })
    }
}

fn assert_created(response: Response) -> Result<(Vec<Header>, Option<String>), DockerError> {
    match response {
        Response::Okay { headers: _, body } => Err(DockerError::UnexpectedResponseError {
            status: 200,
            body,
            message: "expected CREATED, but recieved OK".to_string(),
        }),
        Response::Created { headers, body } => Ok((headers, body)),
        Response::NoContent { .. } => Err(DockerError::UnexpectedResponseError {
            status: 204,
            body: None,
            message: "expected CREATED, but recieved NO CONTENT".to_string(),
        }),
        Response::Error { status: 404, .. } => Err(DockerError::NotFoundError),
        Response::Error { status, body, .. } => Err(DockerError::UnexpectedResponseError {
            status,
            body,
            message: "non successful response".to_string(),
        }),
    }
}
