use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

// Like NON_ALPHANUMERIC, except allow minus, period, and underscore
const FS_NON_ALPHANUMERIC: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~');

pub fn safe_file_system_name(name: &str) -> String {
    utf8_percent_encode(&name, FS_NON_ALPHANUMERIC).to_string()
}

pub fn safe_prefixed_credential_name(name: &str) -> (String, String) {
    let mut service_name = name.to_string();
    // keep credential names down to 32 characters, that seems the most portable
    service_name.truncate(27);

    let mut chars = service_name.chars();
    let mut buffer = String::new();

    if let Some(first) = chars.next() {
        buffer.push(match first {
            'a'..='z' | '_' => first,
            'A'..='Z' => first.to_ascii_lowercase(),
            _ => '-',
        });
    }
    for ch in chars {
        buffer.push(match ch {
            'a'..='z' | '0'..='9' | '-' | '_' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            _ => '-',
        });
    }

    let result = format!("doug-{}", buffer).to_string();
    (result.clone(), result)
}

#[cfg(test)]
mod tests {
    mod safe_file_system_name {
        use super::super::safe_file_system_name;

        #[test]
        fn should_allow_period_minus_an_underscore() {
            let given = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
            let actual = safe_file_system_name(given);
            assert_eq!(
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789%20%21%22%23%24%25%26%27%28%29%2A%2B%2C-.%2F%3A%3B%3C%3D%3E%3F%40%5B%5C%5D%5E_%60%7B%7C%7D%7E",
                actual,
            );
        }
    }
    mod safe_prefixed_directory_name {
        use super::super::safe_prefixed_credential_name;

        #[test]
        fn should_enforce_first_character_as_alpha() {
            let given = "?foo";
            let (actual_user_name, actual_group_name) = safe_prefixed_credential_name(given);

            assert_eq!("doug--foo", actual_user_name);
            assert_eq!("doug--foo", actual_group_name);
        }

        #[test]
        fn should_only_output_lowercase() {
            let given = "ABCdef";
            let (actual_user_name, actual_group_name) = safe_prefixed_credential_name(given);

            assert_eq!("doug-abcdef", actual_user_name);
            assert_eq!("doug-abcdef", actual_group_name);
        }

        #[test]
        fn should_allow_numbers_minus_and_underscore() {
            let given = "pi3_14-1";
            let (actual_user_name, actual_group_name) = safe_prefixed_credential_name(given);

            assert_eq!("doug-pi3_14-1", actual_user_name);
            assert_eq!("doug-pi3_14-1", actual_group_name);
        }

        #[test]
        fn should_truncate_name_to_27_characters() {
            let given = "abcdefghijklmnopqrstuvwxyz_EXTRA";
            let (actual_user_name, actual_group_name) = safe_prefixed_credential_name(given);

            assert_eq!("doug-abcdefghijklmnopqrstuvwxyz_", actual_user_name);
            assert_eq!("doug-abcdefghijklmnopqrstuvwxyz_", actual_group_name);
        }
    }
}
