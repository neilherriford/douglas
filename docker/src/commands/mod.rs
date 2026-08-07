pub(crate) mod container;
pub(crate) mod image;
pub(crate) mod json_parser;
pub(crate) mod system;

use crate::DockerError;
use serde_json::Value as Json;

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

            return Err(DockerError::GeneralError(msg));
        }
    }

    Ok(())
}
