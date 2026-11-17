#[cfg(any(test, target_os = "linux"))]
use anyhow::Context;
use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessIdentityCheck {
    NotRecorded,
    Matches,
    DifferentProcess,
    Unavailable,
}

impl ProcessIdentityCheck {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotRecorded => "not_recorded",
            Self::Matches => "matches",
            Self::DifferentProcess => "different_process",
            Self::Unavailable => "unavailable",
        }
    }
}

pub(crate) fn capture(process_id: u32) -> Result<ProcessIdentity> {
    if process_id == 0 {
        bail!("Process ID must be greater than zero");
    }
    capture_platform(process_id)
}

pub(crate) fn compare(
    process_id: Option<u32>,
    recorded_fingerprint: Option<&str>,
) -> ProcessIdentityCheck {
    let (Some(process_id), Some(recorded_fingerprint)) = (process_id, recorded_fingerprint) else {
        return ProcessIdentityCheck::NotRecorded;
    };
    match capture(process_id) {
        Ok(current) if current.fingerprint == recorded_fingerprint => ProcessIdentityCheck::Matches,
        Ok(_) => ProcessIdentityCheck::DifferentProcess,
        Err(_) => ProcessIdentityCheck::Unavailable,
    }
}

#[cfg(target_os = "linux")]
fn capture_platform(process_id: u32) -> Result<ProcessIdentity> {
    use std::fs;

    let stat_path = format!("/proc/{process_id}/stat");
    let stat = fs::read_to_string(&stat_path)
        .with_context(|| format!("Cannot read process identity from {stat_path}"))?;
    let (reported_pid, process_group_id, start_time_ticks) =
        parse_linux_stat(&stat).context("Cannot parse Linux process identity")?;
    if reported_pid != process_id {
        bail!("Linux process metadata PID does not match the requested PID");
    }
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .context("Cannot read Linux boot identity")?;
    let boot_id = normalize_linux_boot_id(&boot_id)?;
    Ok(ProcessIdentity {
        fingerprint: format!(
            "linux-proc-v1:{boot_id}:{reported_pid}:{process_group_id}:{start_time_ticks}"
        ),
    })
}

#[cfg(not(target_os = "linux"))]
fn capture_platform(_process_id: u32) -> Result<ProcessIdentity> {
    bail!("A supported public process identity source is unavailable on this platform")
}

#[cfg(any(test, target_os = "linux"))]
fn parse_linux_stat(value: &str) -> Result<(u32, u32, u64)> {
    let open_paren = value
        .find('(')
        .context("Linux process stat record has no command field")?;
    let close_paren = value
        .rfind(')')
        .filter(|index| *index > open_paren)
        .context("Linux process stat record has no command terminator")?;
    let process_id = value[..open_paren]
        .trim()
        .parse::<u32>()
        .context("Linux process stat record has an invalid PID")?;
    let fields = value[close_paren + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let process_group_id = fields
        .get(2)
        .context("Linux process stat record has no process-group field")?
        .parse::<u32>()
        .context("Linux process stat record has an invalid process-group ID")?;
    let start_time_ticks = fields
        .get(19)
        .context("Linux process stat record has no start-time field")?
        .parse::<u64>()
        .context("Linux process stat record has an invalid start time")?;
    if process_id == 0 || process_group_id == 0 {
        bail!("Linux process and process-group IDs must be greater than zero");
    }
    Ok((process_id, process_group_id, start_time_ticks))
}

#[cfg(any(test, target_os = "linux"))]
fn normalize_linux_boot_id(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid {
        bail!("Linux boot identity is malformed");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{ProcessIdentityCheck, normalize_linux_boot_id, parse_linux_stat};

    #[test]
    fn parses_linux_pid_group_and_start_time_after_parenthesized_command() {
        let mut fields = vec!["S", "1", "456", "456"];
        while fields.len() < 19 {
            fields.push("0");
        }
        fields.push("987654");
        let stat = format!("42 (agy (worker) child) {}", fields.join(" "));

        assert_eq!(
            parse_linux_stat(&stat).expect("parse process stat"),
            (42, 456, 987654)
        );
    }

    #[test]
    fn rejects_linux_stat_records_with_missing_or_invalid_identity_fields() {
        assert!(parse_linux_stat("not a process record").is_err());
        assert!(parse_linux_stat("42 (agy) S 1 0 0").is_err());
    }

    #[test]
    fn validates_and_normalizes_linux_boot_id() {
        assert_eq!(
            normalize_linux_boot_id("550E8400-E29B-41D4-A716-446655440000\n")
                .expect("normalize boot ID"),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert!(normalize_linux_boot_id("not-a-boot-id").is_err());
    }

    #[test]
    fn process_identity_statuses_remain_explicit() {
        assert_eq!(ProcessIdentityCheck::Unavailable.as_str(), "unavailable");
        assert_eq!(ProcessIdentityCheck::NotRecorded.as_str(), "not_recorded");
        assert_eq!(
            ProcessIdentityCheck::DifferentProcess.as_str(),
            "different_process"
        );
        assert_eq!(ProcessIdentityCheck::Matches.as_str(), "matches");
    }

    #[test]
    fn missing_recorded_process_identity_stays_unresolved() {
        assert_eq!(
            super::compare(None, None),
            ProcessIdentityCheck::NotRecorded
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn captures_the_current_linux_process_identity() {
        let process_id = std::process::id();
        let identity = super::capture(process_id).expect("capture current Linux process");

        assert!(identity.fingerprint.starts_with("linux-proc-v1:"));
        assert_eq!(
            super::compare(Some(process_id), Some(&identity.fingerprint)),
            ProcessIdentityCheck::Matches
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_platform_does_not_claim_process_identity() {
        assert!(super::capture(1).is_err());
    }
}
