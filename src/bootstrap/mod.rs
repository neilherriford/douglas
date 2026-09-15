use blueprint::{listener::LivenessCheck, service::ServiceDefinition};
use config::DouglasFolders;
use thiserror::Error;

pub mod core_seedlings;
pub mod openbao;
pub mod stop;
pub mod system;

#[derive(Error, Debug)]
pub enum LivenessCheckError {
    #[error("Service '{0}' has no configured liveness check")]
    MissingLivenessCheck(String),
    #[error("Unknown service '{0}'")]
    UnknownService(String),
}

pub(crate) fn liveness_check(
    service_name: &str,
    douglas_folders: &DouglasFolders,
) -> Result<LivenessCheck, LivenessCheckError> {
    let definition = if service_name == config::services::BRACT {
        bract::service_definition(douglas_folders)
    } else if service_name == config::services::SEEDBANK {
        seedbank::service_definition(douglas_folders)
    } else if service_name == config::services::RESIN {
        resin::service_definition(douglas_folders)
    } else if service_name == config::services::WOODWARD {
        woodward::service_definition(douglas_folders)
    } else {
        return Err(LivenessCheckError::UnknownService(service_name.to_string()));
    };

    require_liveness(&definition, service_name)
}

fn require_liveness(
    definition: &ServiceDefinition,
    service_name: &str,
) -> Result<LivenessCheck, LivenessCheckError> {
    definition
        .liveness
        .clone()
        .ok_or_else(|| LivenessCheckError::MissingLivenessCheck(service_name.to_string()))
}
