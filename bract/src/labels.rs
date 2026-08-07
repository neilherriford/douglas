use std::{str::FromStr, sync::LazyLock};
use thiserror::Error;

static VERSION_LABEL: &str = "douglas.version";
static VERSION_LABEL_KEY: LazyLock<docker_types::LabelKey> =
    LazyLock::new(|| docker_types::LabelKey::from_str(VERSION_LABEL).expect("valid label key"));

#[derive(Error, Debug)]
pub enum LabelError {
    #[error("Could not determine resource version")]
    MissingVersion,
}

pub fn get_version(
    labels: Vec<docker_types::Label>,
) -> Result<seedbank_types::Version, LabelError> {
    labels
        .iter()
        .find(|label| label.name == *VERSION_LABEL_KEY)
        .ok_or(LabelError::MissingVersion)?
        .value
        .parse::<seedbank_types::Version>()
        .map_err(|_| LabelError::MissingVersion)
}

pub fn create_version_label(version: &seedbank_types::Version) -> docker_types::Label {
    docker_types::Label::new(VERSION_LABEL, &version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_version_should_round_trip_through_create_version_label() {
        let version = seedbank_types::Version(2);
        let label = create_version_label(&version);

        let result = get_version(vec![label]).expect("should find the version");

        assert_eq!(result, version);
    }

    #[test]
    fn test_get_version_should_error_when_the_label_is_missing() {
        let result = get_version(vec![docker_types::Label::new("other.label", "1")]);

        assert!(matches!(result, Err(LabelError::MissingVersion)));
    }

    #[test]
    fn test_get_version_should_error_when_the_value_is_not_a_version() {
        let result = get_version(vec![docker_types::Label::new(
            VERSION_LABEL,
            "not-a-number",
        )]);

        assert!(matches!(result, Err(LabelError::MissingVersion)));
    }
}
