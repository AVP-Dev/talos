use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitKind {
    /// Short-term request/token rate limit (RPM/TPM, HTTP 429)
    RequestLimit,
    /// Quota or billing limit exhausted
    QuotaExhausted,
    /// Provider capacity overloaded
    Overloaded,
}

impl RateLimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestLimit => "request_rate_limit",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Overloaded => "server_overloaded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitInfo {
    pub kind: RateLimitKind,
    pub wait_duration: Duration,
    pub matched_signature: String,
    pub extracted_time: bool,
}

/// Known rate limit signatures across OpenAI/Codex, Anthropic/Claude, Gemini/Agy, and local/proxy gateways.
const REQUEST_LIMIT_SIGNATURES: &[&str] = &[
    "rate limit reached",
    "rate_limit_exceeded",
    "rate_limit_error",
    "too many requests",
    "http 429",
    "status: 429",
    "status code: 429",
    "429 too many requests",
    "ratelimitexceeded",
    "resource_exhausted",
    "requests per minute",
    "tokens per minute",
    "tpm limit",
    "rpm limit",
    "rate limit exceeded",
];

const QUOTA_SIGNATURES: &[&str] = &[
    "insufficient_quota",
    "quota exceeded",
    "you exceeded your current quota",
    "billing details",
    "quota_exceeded",
    "usage limit exceeded",
    "credit balance is too low",
    "out of credits",
];

const OVERLOADED_SIGNATURES: &[&str] = &[
    "overloaded",
    "server is temporarily overloaded",
    "capacity exceeded",
    "service unavailable",
    "http 529",
    "status: 529",
    "model is currently overloaded",
];

/// Analyzes an error message / stderr / API response and determines whether it represents
/// a rate limit or quota exhaustion, calculating the recommended sleep duration.
pub fn detect_rate_limit(error_text: &str, attempt_number: usize) -> Option<RateLimitInfo> {
    let lower = error_text.to_lowercase();

    // 1. Check for request rate limits
    for sig in REQUEST_LIMIT_SIGNATURES {
        if lower.contains(sig) {
            let (wait, extracted) = extract_wait_duration(&lower)
                .unwrap_or_else(|| (adaptive_request_backoff(attempt_number), false));
            return Some(RateLimitInfo {
                kind: RateLimitKind::RequestLimit,
                wait_duration: wait,
                matched_signature: (*sig).to_owned(),
                extracted_time: extracted,
            });
        }
    }

    // 2. Check for quota exhaustion
    for sig in QUOTA_SIGNATURES {
        if lower.contains(sig) {
            let (wait, extracted) = extract_wait_duration(&lower)
                .unwrap_or_else(|| (adaptive_quota_backoff(attempt_number), false));
            return Some(RateLimitInfo {
                kind: RateLimitKind::QuotaExhausted,
                wait_duration: wait,
                matched_signature: (*sig).to_owned(),
                extracted_time: extracted,
            });
        }
    }

    // 3. Check for server overloaded
    for sig in OVERLOADED_SIGNATURES {
        if lower.contains(sig) {
            let (wait, extracted) = extract_wait_duration(&lower)
                .unwrap_or_else(|| (adaptive_overloaded_backoff(attempt_number), false));
            return Some(RateLimitInfo {
                kind: RateLimitKind::Overloaded,
                wait_duration: wait,
                matched_signature: (*sig).to_owned(),
                extracted_time: extracted,
            });
        }
    }

    None
}

/// Try to parse explicit duration from error string (e.g. "try again in 20s", "resets in 5m30s", "retry-after: 60")
fn extract_wait_duration(text: &str) -> Option<(Duration, bool)> {
    // Check "retry-after: <seconds>"
    if let Some(pos) = text.find("retry-after:") {
        let remainder = &text[pos + "retry-after:".len()..];
        if let Some(val) = extract_first_number(remainder).filter(|&v| v > 0) {
            return Some((Duration::from_secs(val), true));
        }
    }

    // Check "try again in <N> (s|m|h)"
    if let Some(pos) = text.find("try again in") {
        let remainder = &text[pos + "try again in".len()..];
        if let Some(dur) = parse_time_expression(remainder) {
            return Some((dur, true));
        }
    }

    // Check "resets in <N> (s|m|h)"
    if let Some(pos) = text.find("resets in") {
        let remainder = &text[pos + "resets in".len()..];
        if let Some(dur) = parse_time_expression(remainder) {
            return Some((dur, true));
        }
    }

    // Check "wait <N> (s|m|h)"
    if let Some(pos) = text.find("wait ") {
        let remainder = &text[pos + "wait ".len()..];
        if let Some(dur) = parse_time_expression(remainder) {
            return Some((dur, true));
        }
    }

    // Check "retry after <N> (s|m|h)"
    if let Some(pos) = text.find("retry after") {
        let remainder = &text[pos + "retry after".len()..];
        if let Some(dur) = parse_time_expression(remainder) {
            return Some((dur, true));
        }
    }

    None
}

fn extract_first_number(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let num_str: String = trimmed
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse::<u64>().ok()
}

fn parse_time_expression(text: &str) -> Option<Duration> {
    let trimmed = text.trim();
    let words: Vec<&str> = trimmed.split_whitespace().take(4).collect();
    if words.is_empty() {
        return None;
    }

    let first = words[0].trim_matches(|c: char| !c.is_alphanumeric());
    // Cases like "20s", "5m", "1h", "45ms"
    if let Some(num) = first
        .strip_suffix("ms")
        .and_then(|val| val.parse::<u64>().ok())
    {
        return Some(Duration::from_millis(num));
    }
    if let Some(num) = first
        .strip_suffix('s')
        .and_then(|val| val.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(num));
    }
    if let Some(num) = first
        .strip_suffix('m')
        .and_then(|val| val.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(num * 60));
    }
    if let Some(num) = first
        .strip_suffix('h')
        .and_then(|val| val.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(num * 3600));
    }

    // Cases like "20 seconds", "5 minutes", "1 hour"
    if let Ok(num) = first.parse::<u64>() {
        if words.len() > 1 {
            let unit = words[1].trim_matches(|c: char| !c.is_alphabetic());
            match unit {
                "s" | "sec" | "second" | "seconds" => return Some(Duration::from_secs(num)),
                "m" | "min" | "minute" | "minutes" => return Some(Duration::from_secs(num * 60)),
                "h" | "hr" | "hour" | "hours" => return Some(Duration::from_secs(num * 3600)),
                _ => return Some(Duration::from_secs(num)),
            }
        } else {
            return Some(Duration::from_secs(num));
        }
    }

    None
}

/// Adaptive backoff for short-term RPM/TPM rate limits (seconds to minutes)
fn adaptive_request_backoff(attempt_number: usize) -> Duration {
    match attempt_number {
        0 | 1 => Duration::from_secs(20),
        2 => Duration::from_secs(60),
        3 => Duration::from_secs(120),
        4 => Duration::from_secs(300),
        _ => Duration::from_secs(600), // Max 10 minutes
    }
}

/// Adaptive backoff for quota exhaustion (minutes)
fn adaptive_quota_backoff(attempt_number: usize) -> Duration {
    match attempt_number {
        0 | 1 => Duration::from_secs(5 * 60), // 5 minutes
        2 => Duration::from_secs(15 * 60),    // 15 minutes
        3 => Duration::from_secs(30 * 60),    // 30 minutes
        _ => Duration::from_secs(45 * 60),    // 45 minutes
    }
}

/// Adaptive backoff for overloaded server
fn adaptive_overloaded_backoff(attempt_number: usize) -> Duration {
    match attempt_number {
        0 | 1 => Duration::from_secs(15),
        2 => Duration::from_secs(45),
        3 => Duration::from_secs(90),
        _ => Duration::from_secs(180),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_openai_rate_limit_with_seconds() {
        let err = "Error 429: Rate limit reached for requests. Please try again in 20s.";
        let info = detect_rate_limit(err, 1).expect("rate limit detected");
        assert_eq!(info.kind, RateLimitKind::RequestLimit);
        assert_eq!(info.wait_duration, Duration::from_secs(20));
        assert!(info.extracted_time);
    }

    #[test]
    fn detects_openai_rate_limit_with_minutes() {
        let err = "Rate limit exceeded. Please try again in 3 minutes.";
        let info = detect_rate_limit(err, 1).expect("rate limit detected");
        assert_eq!(info.kind, RateLimitKind::RequestLimit);
        assert_eq!(info.wait_duration, Duration::from_secs(180));
        assert!(info.extracted_time);
    }

    #[test]
    fn detects_anthropic_resets_in() {
        let err = "anthropic.RateLimitError: 429 Too Many Requests, resets in 45s";
        let info = detect_rate_limit(err, 1).expect("rate limit detected");
        assert_eq!(info.kind, RateLimitKind::RequestLimit);
        assert_eq!(info.wait_duration, Duration::from_secs(45));
        assert!(info.extracted_time);
    }

    #[test]
    fn detects_gemini_resource_exhausted() {
        let err = "RESOURCE_EXHAUSTED: Quota exceeded for quota metric 'GenerateContent'";
        let info = detect_rate_limit(err, 1).expect("rate limit detected");
        assert_eq!(info.kind, RateLimitKind::RequestLimit);
        assert_eq!(info.wait_duration, Duration::from_secs(20)); // fallback attempt 1
        assert!(!info.extracted_time);
    }

    #[test]
    fn detects_openai_insufficient_quota_adaptive() {
        let err = "You exceeded your current quota, please check your plan and billing details.";
        let info = detect_rate_limit(err, 2).expect("quota detected");
        assert_eq!(info.kind, RateLimitKind::QuotaExhausted);
        assert_eq!(info.wait_duration, Duration::from_secs(15 * 60)); // 15 min on attempt 2
    }

    #[test]
    fn ignores_unrelated_code_errors() {
        let err = "SyntaxError: Unexpected identifier 'safe_eval' at line 42";
        assert!(detect_rate_limit(err, 1).is_none());

        let err2 = "AssertionError: 4 != 5 in test_calc.py";
        assert!(detect_rate_limit(err2, 1).is_none());
    }
}
