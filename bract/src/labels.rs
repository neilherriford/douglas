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

pub fn get_version(labels: &[docker_types::Label]) -> Result<seedbank_types::Version, LabelError> {
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

static ORIGIN_LABEL: &str = "douglas.origin";
static ORIGIN_LABEL_KEY: LazyLock<docker_types::LabelKey> =
    LazyLock::new(|| docker_types::LabelKey::from_str(ORIGIN_LABEL).expect("valid label key"));

pub fn get_origin(labels: &[docker_types::Label]) -> Option<seedbank_types::Origin> {
    let value = &labels
        .iter()
        .find(|label| label.name == *ORIGIN_LABEL_KEY)?
        .value;

    match value.as_str() {
        "core" => Some(seedbank_types::Origin::Core),
        "user" => Some(seedbank_types::Origin::User),
        _ => None,
    }
}

pub fn create_origin_label(origin: seedbank_types::Origin) -> docker_types::Label {
    let value = match origin {
        seedbank_types::Origin::Core => "core",
        seedbank_types::Origin::User => "user",
    };
    docker_types::Label::new(ORIGIN_LABEL, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_version_should_round_trip_through_create_version_label() {
        let version = seedbank_types::Version(2);
        let label = create_version_label(&version);
        let result = get_version(&[label]).expect("should find the version");

        assert_eq!(result, version);
    }

    #[test]
    fn test_get_version_should_error_when_the_label_is_missing() {
        let result = get_version(&[docker_types::Label::new("other.label", "1")]);

        assert!(matches!(result, Err(LabelError::MissingVersion)));
    }

    #[test]
    fn test_get_version_should_error_when_the_value_is_not_a_version() {
        let result = get_version(&[docker_types::Label::new(VERSION_LABEL, "not-a-number")]);

        assert!(matches!(result, Err(LabelError::MissingVersion)));
    }

    #[test]
    fn test_get_origin_should_round_trip_through_create_origin_label() {
        let label = create_origin_label(seedbank_types::Origin::Core);

        let result = get_origin(&[label]);

        assert_eq!(result, Some(seedbank_types::Origin::Core));
    }

    #[test]
    fn test_get_origin_should_return_none_when_the_label_is_missing() {
        let result = get_origin(&[docker_types::Label::new("other.label", "core")]);

        assert_eq!(result, None);
    }

    #[test]
    fn test_get_origin_should_return_none_for_an_unrecognized_value() {
        let result = get_origin(&[docker_types::Label::new(ORIGIN_LABEL, "unknown")]);

        assert_eq!(result, None);
    }
}
