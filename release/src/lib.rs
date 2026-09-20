use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

const LENGTH_LEN: usize = 4;

#[derive(Error, Debug)]
pub enum ReleaseError {
    #[error("release metadata is missing or malformed: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    pub format: u8,
    pub core: BTreeMap<String, u16>,
}

impl ReleaseMetadata {
    pub fn current() -> Self {
        Self {
            format: config::DATA_FORMAT,
            core: config::seedlings::core_versions()
                .iter()
                .map(|(name, version)| ((*name).to_string(), *version))
                .collect(),
        }
    }
}

pub fn attach(payload: &mut Vec<u8>, metadata: &ReleaseMetadata) -> Result<(), ReleaseError> {
    let json =
        serde_json::to_vec(metadata).map_err(|err| ReleaseError::Malformed(err.to_string()))?;
    let length = u32::try_from(json.len())
        .map_err(|_| ReleaseError::Malformed("metadata is too large".to_string()))?;

    payload.extend_from_slice(&json);
    payload.extend_from_slice(&length.to_le_bytes());
    Ok(())
}

pub fn detach(payload: &[u8]) -> Result<(&[u8], ReleaseMetadata), ReleaseError> {
    let length_start = payload
        .len()
        .checked_sub(LENGTH_LEN)
        .ok_or_else(|| ReleaseError::Malformed("missing length".to_string()))?;
    let length_bytes: [u8; LENGTH_LEN] = payload[length_start..]
        .try_into()
        .map_err(|_| ReleaseError::Malformed("missing length".to_string()))?;
    let length = usize::try_from(u32::from_le_bytes(length_bytes))
        .map_err(|_| ReleaseError::Malformed("length does not fit".to_string()))?;

    let json_start = length_start
        .checked_sub(length)
        .ok_or_else(|| ReleaseError::Malformed("length exceeds the payload".to_string()))?;
    let metadata = serde_json::from_slice::<ReleaseMetadata>(&payload[json_start..length_start])
        .map_err(|err| ReleaseError::Malformed(err.to_string()))?;

    Ok((&payload[..json_start], metadata))
}

pub fn strip(payload: &mut Vec<u8>) -> Result<(), ReleaseError> {
    let remaining = detach(payload)?.0.len();
    payload.truncate(remaining);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReleaseMetadata {
        ReleaseMetadata {
            format: 3,
            core: BTreeMap::from([("openbao".to_string(), 2), ("traefik".to_string(), 1)]),
        }
    }

    #[test]
    fn test_current_should_carry_the_config_format_and_every_core_version() {
        let current = ReleaseMetadata::current();

        assert_eq!(current.format, config::DATA_FORMAT);
        assert_eq!(current.core.len(), config::seedlings::core_versions().len());
        assert_eq!(
            current.core.get(config::seedlings::OPENBAO),
            Some(&config::seedlings::OPENBAO_VERSION)
        );
    }

    #[test]
    fn test_detach_should_recover_what_was_attached_and_the_payload_before_it() {
        let mut payload = b"pretend binary bytes".to_vec();
        let Ok(()) = attach(&mut payload, &sample()) else {
            panic!("should attach");
        };

        let Ok((rest, metadata)) = detach(&payload) else {
            panic!("should detach");
        };

        assert_eq!(rest, b"pretend binary bytes");
        assert_eq!(metadata, sample());
    }

    #[test]
    fn test_detach_should_reject_a_payload_with_no_block() {
        assert!(matches!(
            detach(b"a plain payload"),
            Err(ReleaseError::Malformed(_))
        ));
    }

    #[test]
    fn test_detach_should_reject_a_payload_too_short_to_hold_a_length() {
        assert!(matches!(detach(b"ab"), Err(ReleaseError::Malformed(_))));
    }

    #[test]
    fn test_detach_should_reject_a_length_that_exceeds_the_payload() {
        let mut payload = b"short".to_vec();
        payload.extend_from_slice(&1000u32.to_le_bytes());

        assert!(matches!(detach(&payload), Err(ReleaseError::Malformed(_))));
    }

    #[test]
    fn test_detach_should_reject_a_block_that_is_not_valid_metadata() {
        let mut payload = b"binary".to_vec();
        let json = b"{\"format\":\"not a number\"}";
        payload.extend_from_slice(json);
        let Ok(length) = u32::try_from(json.len()) else {
            panic!("length fits");
        };
        payload.extend_from_slice(&length.to_le_bytes());

        assert!(matches!(detach(&payload), Err(ReleaseError::Malformed(_))));
    }

    #[test]
    fn test_strip_should_remove_an_attached_block_and_leave_the_payload() {
        let mut payload = b"pretend binary bytes".to_vec();
        let Ok(()) = attach(&mut payload, &sample()) else {
            panic!("should attach");
        };

        let Ok(()) = strip(&mut payload) else {
            panic!("should strip");
        };

        assert_eq!(payload, b"pretend binary bytes");
    }

    #[test]
    fn test_strip_should_reject_a_payload_with_no_block() {
        let mut payload = b"a plain payload".to_vec();

        assert!(matches!(
            strip(&mut payload),
            Err(ReleaseError::Malformed(_))
        ));
        assert_eq!(payload, b"a plain payload");
    }

    #[test]
    fn test_attach_should_produce_identical_bytes_for_identical_metadata() {
        let mut first = b"binary".to_vec();
        let mut second = b"binary".to_vec();

        let Ok(()) = attach(&mut first, &sample()) else {
            panic!("should attach");
        };
        let Ok(()) = attach(&mut second, &sample()) else {
            panic!("should attach");
        };

        assert_eq!(first, second);
    }
}
