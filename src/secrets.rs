const PRIVATE_KEY_BEGIN: &str = "-----BEGIN ";

/// Replace common credential forms before untrusted agent or command text reaches a report.
pub fn redact_sensitive(value: &str) -> String {
    let mut output = value.to_owned();
    output = redact_private_key(&output);
    output = redact_bearer_tokens(&output);
    output = redact_url_credentials(&output);
    output = redact_prefixed_tokens(&output);
    redact_sensitive_assignments(&output)
}

pub fn contains_sensitive(value: &str) -> bool {
    redact_sensitive(value) != value
}

fn redact_private_key(value: &str) -> String {
    let Some(start) = value.find(PRIVATE_KEY_BEGIN) else {
        return value.to_owned();
    };
    let Some(end_marker) = value[start..].find("-----END ") else {
        return format!("{}[REDACTED PRIVATE KEY]", &value[..start]);
    };
    let end = value[start + end_marker..]
        .find("-----")
        .map(|offset| start + end_marker + offset + 5)
        .unwrap_or(value.len());
    format!("{}[REDACTED PRIVATE KEY]{}", &value[..start], &value[end..])
}

fn redact_bearer_tokens(value: &str) -> String {
    let mut output = value.to_owned();
    let mut offset = 0;
    while let Some(found) = output[offset..].find("Bearer ") {
        let start = offset + found + "Bearer ".len();
        let end = output[start..]
            .find(char::is_whitespace)
            .map(|relative| start + relative)
            .unwrap_or(output.len());
        if end > start {
            output.replace_range(start..end, "[REDACTED]");
            offset = start + "[REDACTED]".len();
        } else {
            offset = end;
        }
    }
    output
}

fn redact_url_credentials(value: &str) -> String {
    let mut output = value.to_owned();
    let mut offset = 0;
    while let Some(scheme) = output[offset..].find("://") {
        let credential_start = offset + scheme + 3;
        let Some(authority_end) = output[credential_start..].find('@') else {
            break;
        };
        let at = credential_start + authority_end;
        if output[credential_start..at].contains(':') {
            output.replace_range(credential_start..at, "[REDACTED]");
            offset = credential_start + "[REDACTED]@".len();
        } else {
            offset = at + 1;
        }
    }
    output
}

fn redact_prefixed_tokens(value: &str) -> String {
    const PREFIXES: &[&str] = &[
        "github_pat_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "AKIA",
        "AIza",
        "eyJ",
    ];
    let mut output = value.to_owned();
    for prefix in PREFIXES {
        let mut offset = 0;
        while let Some(found) = output[offset..].find(prefix) {
            let start = offset + found;
            let token_start = start + prefix.len();
            let end = output[token_start..]
                .find(|character: char| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
                })
                .map(|relative| token_start + relative)
                .unwrap_or(output.len());
            let minimum_length = if *prefix == "AKIA" { 16 } else { 24 };
            if end.saturating_sub(token_start) >= minimum_length {
                output.replace_range(start..end, "[REDACTED]");
                offset = start + "[REDACTED]".len();
            } else {
                offset = end;
            }
        }
    }
    output
}

fn redact_sensitive_assignments(value: &str) -> String {
    const KEYS: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "api-key",
        "access_token",
        "access-token",
        "auth_token",
        "auth-token",
        "private_key",
        "private-key",
        "authorization",
    ];
    let lower = value.to_ascii_lowercase();
    let mut spans = Vec::new();
    for key in KEYS {
        let mut offset = 0;
        while let Some(found) = lower[offset..].find(key) {
            let key_start = offset + found;
            let after_key = key_start + key.len();
            if let Some(separator_offset) = lower.as_bytes()[after_key..]
                .iter()
                .take(16)
                .position(|byte| *byte == b'=' || *byte == b':')
            {
                let separator = after_key + separator_offset;
                let mut token_start = separator + 1;
                while value[token_start..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
                {
                    token_start += value[token_start..]
                        .chars()
                        .next()
                        .map(char::len_utf8)
                        .unwrap_or(0);
                }
                if value[token_start..]
                    .chars()
                    .next()
                    .is_some_and(|character| character == '\'' || character == '"')
                {
                    token_start += 1;
                }
                let token_end = value[token_start..]
                    .find(|character: char| {
                        character.is_whitespace()
                            || matches!(character, '\'' | '"' | ',' | ';' | '}')
                    })
                    .map(|relative| token_start + relative)
                    .unwrap_or(value.len());
                if token_end.saturating_sub(token_start) >= 8 {
                    spans.push((token_start, token_end));
                }
            }
            offset = after_key;
        }
    }
    spans.sort_unstable();
    spans.dedup();
    let mut output = value.to_owned();
    for (start, end) in spans.into_iter().rev() {
        output.replace_range(start..end, "[REDACTED]");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{contains_sensitive, redact_sensitive};

    #[test]
    fn redacts_common_credentials_without_changing_public_text() {
        let value = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456";
        let redacted = redact_sensitive(value);
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(contains_sensitive(value));
        assert_eq!(redact_sensitive("The task is ready."), "The task is ready.");
    }

    #[test]
    fn redacts_token_assignments_and_private_key_blocks() {
        let key_name = ["api", "key"].join("_");
        let value = format!("{key_name}={}", "a".repeat(16));
        let redacted = redact_sensitive(&value);
        assert!(!redacted.contains(&"a".repeat(16)));
        assert!(contains_sensitive(&value));

        let key = format!(
            "{}{}secret{}{}",
            "-----BEGIN ", "PRIVATE KEY-----", "-----END ", "PRIVATE KEY-----"
        );
        assert!(!redact_sensitive(&key).contains("secret"));
    }
}
