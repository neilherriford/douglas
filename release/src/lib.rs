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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallMarker {
    pub version: String,
    pub metadata: ReleaseMetadata,
}

impl InstallMarker {
    pub fn to_json(&self) -> Result<String, ReleaseError> {
        serde_json::to_string(self).map_err(|err| ReleaseError::Malformed(err.to_string()))
    }

    pub fn from_json(raw: &str) -> Result<Self, ReleaseError> {
        serde_json::from_str(raw).map_err(|err| ReleaseError::Malformed(err.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    FormatNewer {
        installed: u8,
        running: u8,
    },
    CoreNewer {
        seedling: String,
        installed: u16,
        running: u16,
    },
    CoreUnknown {
        seedling: String,
        installed: u16,
    },
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Conflict::FormatNewer { installed, running } => write!(
                f,
                "the installed data format is {installed} but this binary only understands format {running}"
            ),
            Conflict::CoreNewer {
                seedling,
                installed,
                running,
            } => write!(
                f,
                "core seedling '{seedling}' is at version {installed} but this binary declares version {running}"
            ),
            Conflict::CoreUnknown {
                seedling,
                installed,
            } => write!(
                f,
                "core seedling '{seedling}' (version {installed}) is installed but this binary does not know it"
            ),
        }
    }
}

pub fn start_conflicts(marker: &InstallMarker, running: &ReleaseMetadata) -> Vec<Conflict> {
    let mut conflicts = Vec::new();

    if marker.metadata.format > running.format {
        conflicts.push(Conflict::FormatNewer {
            installed: marker.metadata.format,
            running: running.format,
        });
    }

    for (seedling, installed) in &marker.metadata.core {
        match running.core.get(seedling) {
            None => conflicts.push(Conflict::CoreUnknown {
                seedling: seedling.clone(),
                installed: *installed,
            }),
            Some(running_version) if installed > running_version => {
                conflicts.push(Conflict::CoreNewer {
                    seedling: seedling.clone(),
                    installed: *installed,
                    running: *running_version,
                });
            }
            Some(_) => {}
        }
    }

    conflicts
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    Format {
        from: u8,
        to: u8,
    },
    CoreVersion {
        seedling: String,
        from: u16,
        to: u16,
    },
    CoreAdded {
        seedling: String,
        version: u16,
    },
    CoreRemoved {
        seedling: String,
        version: u16,
    },
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Difference::Format { from, to } => write!(f, "data format {from} -> {to}"),
            Difference::CoreVersion { seedling, from, to } => {
                write!(f, "core seedling '{seedling}' version {from} -> {to}")
            }
            Difference::CoreAdded { seedling, version } => {
                write!(f, "core seedling '{seedling}' added at version {version}")
            }
            Difference::CoreRemoved { seedling, version } => {
                write!(f, "core seedling '{seedling}' (version {version}) removed")
            }
        }
    }
}

pub fn differences(from: &ReleaseMetadata, to: &ReleaseMetadata) -> Vec<Difference> {
    let mut result = Vec::new();

    if from.format != to.format {
        result.push(Difference::Format {
            from: from.format,
            to: to.format,
        });
    }

    for (seedling, from_version) in &from.core {
        match to.core.get(seedling) {
            None => result.push(Difference::CoreRemoved {
                seedling: seedling.clone(),
                version: *from_version,
            }),
            Some(to_version) if to_version != from_version => {
                result.push(Difference::CoreVersion {
                    seedling: seedling.clone(),
                    from: *from_version,
                    to: *to_version,
                });
            }
            Some(_) => {}
        }
    }

    for (seedling, version) in &to.core {
        if !from.core.contains_key(seedling) {
            result.push(Difference::CoreAdded {
                seedling: seedling.clone(),
                version: *version,
            });
        }
    }

    result
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

    fn metadata(format: u8, openbao: u16, traefik: u16) -> ReleaseMetadata {
        ReleaseMetadata {
            format,
            core: BTreeMap::from([
                ("openbao".to_string(), openbao),
                ("traefik".to_string(), traefik),
            ]),
        }
    }

    fn marker(metadata: ReleaseMetadata) -> InstallMarker {
        InstallMarker {
            version: "0.2.1".to_string(),
            metadata,
        }
    }

    #[test]
    fn test_marker_should_round_trip_through_json() {
        let original = marker(metadata(2, 1, 3));

        let Ok(json) = original.to_json() else {
            panic!("should serialize");
        };
        let Ok(recovered) = InstallMarker::from_json(&json) else {
            panic!("should parse");
        };

        assert_eq!(recovered, original);
    }

    #[test]
    fn test_marker_should_reject_contents_that_are_not_a_marker() {
        assert!(matches!(
            InstallMarker::from_json("not json"),
            Err(ReleaseError::Malformed(_))
        ));
        assert!(matches!(
            InstallMarker::from_json("{\"version\":\"0.2.1\"}"),
            Err(ReleaseError::Malformed(_))
        ));
    }

    #[test]
    fn test_start_conflicts_should_be_empty_when_the_marker_matches_the_running_binary() {
        let conflicts = start_conflicts(&marker(metadata(1, 1, 1)), &metadata(1, 1, 1));

        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_start_conflicts_should_allow_a_binary_newer_than_the_installed_data() {
        let conflicts = start_conflicts(&marker(metadata(1, 1, 1)), &metadata(2, 2, 3));

        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_start_conflicts_should_refuse_a_binary_with_an_older_data_format() {
        let conflicts = start_conflicts(&marker(metadata(3, 1, 1)), &metadata(2, 1, 1));

        assert_eq!(
            conflicts,
            vec![Conflict::FormatNewer {
                installed: 3,
                running: 2
            }]
        );
    }

    #[test]
    fn test_start_conflicts_should_refuse_a_binary_with_an_older_core_seedling_version() {
        let conflicts = start_conflicts(&marker(metadata(1, 2, 1)), &metadata(1, 1, 1));

        assert_eq!(
            conflicts,
            vec![Conflict::CoreNewer {
                seedling: "openbao".to_string(),
                installed: 2,
                running: 1
            }]
        );
    }

    #[test]
    fn test_start_conflicts_should_refuse_a_binary_that_does_not_know_an_installed_seedling() {
        let running = ReleaseMetadata {
            format: 1,
            core: BTreeMap::from([("traefik".to_string(), 1)]),
        };

        let conflicts = start_conflicts(&marker(metadata(1, 1, 1)), &running);

        assert_eq!(
            conflicts,
            vec![Conflict::CoreUnknown {
                seedling: "openbao".to_string(),
                installed: 1
            }]
        );
    }

    #[test]
    fn test_start_conflicts_should_report_every_conflict_in_a_stable_order() {
        let conflicts = start_conflicts(&marker(metadata(3, 2, 2)), &metadata(2, 1, 1));

        assert_eq!(
            conflicts,
            vec![
                Conflict::FormatNewer {
                    installed: 3,
                    running: 2
                },
                Conflict::CoreNewer {
                    seedling: "openbao".to_string(),
                    installed: 2,
                    running: 1
                },
                Conflict::CoreNewer {
                    seedling: "traefik".to_string(),
                    installed: 2,
                    running: 1
                },
            ]
        );
    }

    #[test]
    fn test_conflict_messages_should_name_the_seedling_and_both_versions() {
        let message = Conflict::CoreNewer {
            seedling: "openbao".to_string(),
            installed: 2,
            running: 1,
        }
        .to_string();

        assert!(message.contains("openbao"));
        assert!(message.contains('2'));
        assert!(message.contains('1'));
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

    #[test]
    fn test_differences_should_be_empty_for_identical_metadata() {
        assert!(differences(&metadata(1, 1, 1), &metadata(1, 1, 1)).is_empty());
    }

    #[test]
    fn test_differences_should_report_a_format_change() {
        assert_eq!(
            differences(&metadata(1, 1, 1), &metadata(2, 1, 1)),
            vec![Difference::Format { from: 1, to: 2 }]
        );
    }

    #[test]
    fn test_differences_should_report_a_core_version_change_in_either_direction() {
        assert_eq!(
            differences(&metadata(1, 1, 1), &metadata(1, 2, 1)),
            vec![Difference::CoreVersion {
                seedling: "openbao".to_string(),
                from: 1,
                to: 2
            }]
        );
        assert_eq!(
            differences(&metadata(1, 2, 1), &metadata(1, 1, 1)),
            vec![Difference::CoreVersion {
                seedling: "openbao".to_string(),
                from: 2,
                to: 1
            }]
        );
    }

    #[test]
    fn test_differences_should_report_added_and_removed_seedlings() {
        let fewer = ReleaseMetadata {
            format: 1,
            core: BTreeMap::from([("traefik".to_string(), 1)]),
        };

        assert_eq!(
            differences(&metadata(1, 1, 1), &fewer),
            vec![Difference::CoreRemoved {
                seedling: "openbao".to_string(),
                version: 1
            }]
        );
        assert_eq!(
            differences(&fewer, &metadata(1, 1, 1)),
            vec![Difference::CoreAdded {
                seedling: "openbao".to_string(),
                version: 1
            }]
        );
    }

    #[test]
    fn test_differences_should_report_every_change_in_a_stable_order() {
        assert_eq!(
            differences(&metadata(1, 1, 1), &metadata(2, 2, 3)),
            vec![
                Difference::Format { from: 1, to: 2 },
                Difference::CoreVersion {
                    seedling: "openbao".to_string(),
                    from: 1,
                    to: 2
                },
                Difference::CoreVersion {
                    seedling: "traefik".to_string(),
                    from: 1,
                    to: 3
                },
            ]
        );
    }

    #[test]
    fn test_difference_messages_should_name_what_changed() {
        assert_eq!(
            Difference::Format { from: 1, to: 2 }.to_string(),
            "data format 1 -> 2"
        );
        assert_eq!(
            Difference::CoreVersion {
                seedling: "openbao".to_string(),
                from: 1,
                to: 2
            }
            .to_string(),
            "core seedling 'openbao' version 1 -> 2"
        );
        assert!(
            Difference::CoreAdded {
                seedling: "vault".to_string(),
                version: 1
            }
            .to_string()
            .contains("added")
        );
        assert!(
            Difference::CoreRemoved {
                seedling: "vault".to_string(),
                version: 1
            }
            .to_string()
            .contains("removed")
        );
    }
}
