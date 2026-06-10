pub fn safe_prefixed_credential_name(name: &str) -> String {
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

    format!("doug-{}", buffer).to_string()
}
