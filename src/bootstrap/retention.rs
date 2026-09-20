use crate::verify::Version;
use std::path::{Path, PathBuf};

pub(crate) const RETAINED_COUNT: usize = 3;

const PREFIX: &str = "douglas-";

pub(crate) fn retained_name(version: Version) -> String {
    format!("{PREFIX}{version}")
}

pub(crate) fn retained_path(binary_dir: &Path, version: Version) -> PathBuf {
    binary_dir.join(retained_name(version))
}

pub(crate) fn partial_path(retained: &Path) -> Option<PathBuf> {
    let name = retained.file_name()?;
    Some(retained.with_file_name(format!("{}.partial", name.to_string_lossy())))
}

pub(crate) fn parse_retained(name: &str) -> Option<Version> {
    let rest = name.strip_prefix(PREFIX)?;
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let version = Version {
        major,
        minor,
        patch,
    };
    (retained_name(version) == name).then_some(version)
}

pub(crate) fn expired(names: &[String], keep: usize) -> Vec<String> {
    let mut retained: Vec<(Version, &String)> = names
        .iter()
        .filter_map(|name| parse_retained(name).map(|version| (version, name)))
        .collect();
    retained.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    retained
        .into_iter()
        .skip(keep)
        .map(|(_, name)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(major: u8, minor: u8, patch: u8) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn test_retained_name_should_prefix_the_version() {
        assert_eq!(retained_name(version(0, 2, 11)), "douglas-0.2.11");
    }

    #[test]
    fn test_retained_path_should_sit_in_the_binary_directory() {
        assert_eq!(
            retained_path(
                Path::new("/var/lib/douglas/seedlings/bin"),
                version(0, 0, 2)
            ),
            Path::new("/var/lib/douglas/seedlings/bin/douglas-0.0.2")
        );
    }

    #[test]
    fn test_partial_path_should_sit_next_to_the_retained_binary() {
        assert_eq!(
            partial_path(Path::new("/bin/douglas-0.0.2")),
            Some(PathBuf::from("/bin/douglas-0.0.2.partial"))
        );
    }

    #[test]
    fn test_partial_path_should_be_none_without_a_file_name() {
        assert_eq!(partial_path(Path::new("/")), None);
    }

    #[test]
    fn test_parse_retained_should_recover_the_version() {
        assert_eq!(parse_retained("douglas-1.20.3"), Some(version(1, 20, 3)));
    }

    #[test]
    fn test_parse_retained_should_reject_names_that_are_not_retained_binaries() {
        for name in [
            "douglas",
            "douglas-1.2",
            "douglas-1.2.3.4",
            "douglas-1.2.x",
            "douglas-1.2.3.partial",
            "douglas-01.2.3",
            "douglas-256.0.0",
            "other-1.2.3",
            "",
        ] {
            assert_eq!(parse_retained(name), None, "{name}");
        }
    }

    #[test]
    fn test_expired_should_be_empty_when_within_the_limit() {
        let listing = names(&["douglas-0.0.1", "douglas-0.0.2", "douglas-0.0.3"]);

        assert!(expired(&listing, 3).is_empty());
    }

    #[test]
    fn test_expired_should_name_the_oldest_beyond_the_limit() {
        let listing = names(&[
            "douglas-0.0.2",
            "douglas-0.0.5",
            "douglas-0.0.1",
            "douglas-0.0.4",
            "douglas-0.0.3",
        ]);

        assert_eq!(
            expired(&listing, 3),
            vec!["douglas-0.0.2".to_string(), "douglas-0.0.1".to_string()]
        );
    }

    #[test]
    fn test_expired_should_order_by_version_number_not_by_text() {
        let listing = names(&["douglas-0.0.9", "douglas-0.0.10", "douglas-0.0.11"]);

        assert_eq!(expired(&listing, 2), vec!["douglas-0.0.9".to_string()]);
    }

    #[test]
    fn test_expired_should_ignore_names_that_are_not_retained_binaries() {
        let listing = names(&[
            "douglas",
            "douglas-0.0.1.partial",
            "douglas-0.0.1",
            "douglas-0.0.2",
            "notes.txt",
        ]);

        assert_eq!(expired(&listing, 1), vec!["douglas-0.0.1".to_string()]);
    }

    #[test]
    fn test_expired_should_name_everything_when_keeping_none() {
        let listing = names(&["douglas-0.0.1", "douglas-0.0.2"]);

        assert_eq!(expired(&listing, 0).len(), 2);
    }
}
