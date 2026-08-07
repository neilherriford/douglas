use sha2::{Digest, Sha256};

const PREFIX: &str = "doug-";
const HASH_HEX_LEN: usize = 8;
const MAX_SYSTEM_NAME_LENGTH: usize = 32;
const NORMALIZED_LEN: usize = MAX_SYSTEM_NAME_LENGTH - PREFIX.len() - 1 - HASH_HEX_LEN;

pub(crate) fn derive_system_name(full_name: &str) -> String {
    let normalized = normalize(full_name);
    let truncated: String = normalized.chars().take(NORMALIZED_LEN).collect();
    let hash = short_hash(full_name);
    format!("{PREFIX}{truncated}-{hash}")
}

pub(crate) fn salted(full_name: &str, attempt: u32) -> String {
    if attempt == 0 {
        full_name.to_string()
    } else {
        format!("{full_name}#{attempt}")
    }
}

fn normalize(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'a'..='z' | '0'..='9' | '-' | '_' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            _ => '-',
        })
        .collect()
}

fn short_hash(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    digest
        .iter()
        .take(HASH_HEX_LEN / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod derive_system_name {
        use super::*;

        #[test]
        fn test_should_stay_within_the_max_system_name_length() {
            let name = "a".repeat(200);

            assert!(derive_system_name(&name).len() <= MAX_SYSTEM_NAME_LENGTH);
        }

        #[test]
        fn test_should_be_deterministic_for_the_same_input() {
            assert_eq!(derive_system_name("traefik"), derive_system_name("traefik"));
        }

        #[test]
        fn test_should_differ_for_different_inputs() {
            assert_ne!(derive_system_name("traefik"), derive_system_name("openbao"));
        }

        #[test]
        fn test_should_be_prefixed_with_doug() {
            assert!(derive_system_name("traefik").starts_with("doug-"));
        }

        #[test]
        fn test_should_lowercase_and_normalize_invalid_characters() {
            let name = derive_system_name("Traefik Web!");

            assert!(
                name.chars()
                    .all(|ch| matches!(ch, 'a'..='z' | '0'..='9' | '-' | '_'))
            );
        }

        #[test]
        fn test_should_produce_different_names_for_a_short_prefix_collision() {
            let long_a = format!("{}-a", "x".repeat(30));
            let long_b = format!("{}-b", "x".repeat(30));

            assert_ne!(derive_system_name(&long_a), derive_system_name(&long_b));
        }
    }

    mod salted {
        use super::*;

        #[test]
        fn test_attempt_zero_should_return_the_original_name() {
            assert_eq!(salted("traefik", 0), "traefik");
        }

        #[test]
        fn test_later_attempts_should_differ_from_the_original_and_each_other() {
            let first = salted("traefik", 1);
            let second = salted("traefik", 2);

            assert_ne!(first, "traefik");
            assert_ne!(second, "traefik");
            assert_ne!(first, second);
        }
    }
}
