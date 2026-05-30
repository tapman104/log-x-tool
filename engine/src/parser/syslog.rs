//! Parser for BSD/Linux syslog lines.
//!
//! Expected line structure:
//! ```text
//! Jan 15 14:23:05 hostname process[pid]: message
//! ```
//! The timestamp is the leading `MMM (D)D HH:MM:SS` token.  Everything after
//! the first `: ` following the process field is considered the message portion
//! and is used for level re-detection.

use crate::types::{LogFile, LogLevel};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// ---------------------------------------------------------------------------
// Timestamp extraction
// ---------------------------------------------------------------------------

/// Validate digit positions by indexing bytes directly — no regex, no alloc.
#[inline]
fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// Try to extract the syslog timestamp prefix from `s` (already trimmed).
///
/// Accepts both zero-padded (`Jan 05`) and space-padded (`Jan  5`) day fields.
///
/// Returns a slice of `s` covering exactly `MMM (D)D HH:MM:SS`, or `None` if
/// the line does not start with a valid syslog timestamp.
pub(super) fn try_parse_timestamp(s: &str) -> Option<&str> {
    // Minimum length: "Jan  5 14:23:05" = 15 chars (space-padded single digit)
    //                 "Jan 15 14:23:05" = 15 chars (two-digit day)
    if s.len() < 15 {
        return None;
    }

    // Month abbreviation
    if !MONTHS.contains(&&s[..3]) {
        return None;
    }

    let b = s.as_bytes();

    // Position 3 must be a space
    if b[3] != b' ' {
        return None;
    }

    // Day field: space-padded single digit ("  5") or two-digit ("15")
    let (day_start, day_end) = if b[4] == b' ' {
        // " D" — single digit at position 5
        if s.len() < 16 || !is_digit(b[5]) {
            return None;
        }
        (4, 6) // keep the leading space as part of the token for fidelity
    } else if is_digit(b[4]) {
        if !is_digit(b[5]) {
            return None;
        }
        (4, 6)
    } else {
        return None;
    };

    // Space between day and time
    if b[day_end] != b' ' {
        return None;
    }

    let t = day_end + 1; // start of HH:MM:SS

    // Need at least 8 chars for "HH:MM:SS"
    if t + 8 > s.len() {
        return None;
    }

    if !is_digit(b[t]) || !is_digit(b[t + 1]) { return None; }
    if b[t + 2] != b':'                        { return None; }
    if !is_digit(b[t + 3]) || !is_digit(b[t + 4]) { return None; }
    if b[t + 5] != b':'                        { return None; }
    if !is_digit(b[t + 6]) || !is_digit(b[t + 7]) { return None; }

    let end = t + 8;

    // Must be followed by whitespace or end-of-string — reject mid-word matches
    if end < s.len() && !s.as_bytes()[end].is_ascii_whitespace() {
        return None;
    }

    let _ = day_start; // used implicitly via slice bound below
    Some(&s[..end])
}

// ---------------------------------------------------------------------------
// Message extraction
// ---------------------------------------------------------------------------

/// Return the message portion of a syslog line (everything after `": "`).
///
/// Syslog structure after the timestamp:
/// ```text
///  hostname process[pid]: message
/// ```
/// We skip past the timestamp token and find the first `: ` to locate the
/// message boundary.  Returns `None` if the structure is not recognisable.
fn extract_message<'a>(raw: &'a str, timestamp: &str) -> Option<&'a str> {
    // Skip the timestamp token.  `timestamp` is a slice of `raw`, so its
    // length is a safe byte offset into `raw`.
    let after_ts = raw[timestamp.len()..].trim_start();

    // Find the first ": " which separates "host process[pid]" from the message.
    after_ts.find(": ").map(|pos| after_ts[pos + 2..].trim_start())
}

// ---------------------------------------------------------------------------
// Level detection (mirrors loader::detect_level, message-scoped)
// ---------------------------------------------------------------------------

/// Detect a [`LogLevel`] from a substring using case-insensitive keyword scan.
///
/// Priority order is highest-to-lowest so the dominant keyword wins when
/// multiple level words appear on the same line.
fn detect_level_in(text: &str) -> LogLevel {
    let lower = text.to_lowercase();
    if lower.contains("error") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else if lower.contains("info") {
        LogLevel::Info
    } else if lower.contains("debug") {
        LogLevel::Debug
    } else if lower.contains("trace") {
        LogLevel::Trace
    } else {
        LogLevel::Unknown
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse a syslog-format [`LogFile`] in place.
///
/// For each entry:
/// - Sets `entry.timestamp` from the leading `MMM (D)D HH:MM:SS` prefix.
/// - If `entry.level` is `None` or [`LogLevel::Unknown`], re-detects the level
///   from the *message portion only* (after `hostname process[pid]: `), so that
///   hostnames or process names containing level keywords do not pollute the
///   result.
/// - `entry.raw` is never modified.
pub fn parse(log_file: &mut LogFile) {
    for entry in &mut log_file.entries {
        let line = entry.raw.trim_start();

        // 1. Timestamp
        if let Some(ts) = try_parse_timestamp(line) {
            if entry.timestamp.is_none() {
                entry.timestamp = Some(ts.to_owned());
            }

            // 2. Level refinement — only when the loader left None or Unknown
            let needs_level = matches!(&entry.level, None | Some(LogLevel::Unknown));
            if needs_level {
                if let Some(msg) = extract_message(line, ts) {
                    entry.level = Some(detect_level_in(msg));
                }
            }
        }
    }
}

pub(super) fn parse_line(line: &str) -> crate::types::LineRecord {
    let mut record = crate::types::LineRecord {
        level: LogLevel::Unknown,
        ts_start: 0,
        ts_len: 0,
    };
    
    // First, do a general loader-style detection.
    let lower = line.to_lowercase();
    if lower.contains("error") {
        record.level = LogLevel::Error;
    } else if lower.contains("warn") {
        record.level = LogLevel::Warn;
    } else if lower.contains("info") {
        record.level = LogLevel::Info;
    } else if lower.contains("debug") {
        record.level = LogLevel::Debug;
    } else if lower.contains("trace") {
        record.level = LogLevel::Trace;
    }

    let trimmed = line.trim_start();
    if let Some(ts) = try_parse_timestamp(trimmed) {
        let offset = ts.as_ptr() as usize - line.as_ptr() as usize;
        record.ts_start = offset as u16;
        record.ts_len = ts.len() as u8;

        // Refine level from message portion only (syslog behavior)
        if let Some(msg) = extract_message(trimmed, ts) {
            let msg_lower = msg.to_lowercase();
            if msg_lower.contains("error") {
                record.level = LogLevel::Error;
            } else if msg_lower.contains("warn") {
                record.level = LogLevel::Warn;
            } else if msg_lower.contains("info") {
                record.level = LogLevel::Info;
            } else if msg_lower.contains("debug") {
                record.level = LogLevel::Debug;
            } else if msg_lower.contains("trace") {
                record.level = LogLevel::Trace;
            }
        }
    }

    record
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogEntry, LogFile, LogLevel};
    use std::path::PathBuf;

    // ---- try_parse_timestamp ----

    #[test]
    fn timestamp_two_digit_day() {
        assert_eq!(
            try_parse_timestamp("Jan 15 14:23:05 host proc[1]: msg"),
            Some("Jan 15 14:23:05")
        );
    }

    #[test]
    fn timestamp_space_padded_day() {
        assert_eq!(
            try_parse_timestamp("Jan  5 14:23:05 host proc[1]: msg"),
            Some("Jan  5 14:23:05")
        );
    }

    #[test]
    fn timestamp_all_months() {
        for month in MONTHS {
            let line = format!("{month} 01 00:00:00 h p: m");
            assert!(
                try_parse_timestamp(&line).is_some(),
                "month {month} should parse"
            );
        }
    }

    #[test]
    fn timestamp_rejects_bad_month() {
        assert_eq!(try_parse_timestamp("Xyz 15 14:23:05 host p: msg"), None);
    }

    #[test]
    fn timestamp_rejects_bad_time_separator() {
        assert_eq!(try_parse_timestamp("Jan 15 14-23-05 host p: msg"), None);
    }

    #[test]
    fn timestamp_rejects_too_short() {
        assert_eq!(try_parse_timestamp("Jan 5"), None);
    }

    // ---- extract_message ----

    #[test]
    fn message_extraction_basic() {
        let raw = "Jan 15 14:23:05 myhost myproc[42]: something went wrong";
        let ts = try_parse_timestamp(raw).unwrap();
        assert_eq!(
            extract_message(raw, ts),
            Some("something went wrong")
        );
    }

    #[test]
    fn message_extraction_no_colon_space() {
        let raw = "Jan 15 14:23:05 myhost myproc no-colon-space";
        let ts = try_parse_timestamp(raw).unwrap();
        assert_eq!(extract_message(raw, ts), None);
    }

    // ---- detect_level_in ----

    #[test]
    fn level_error_wins() {
        assert_eq!(detect_level_in("ERROR in info handler"), LogLevel::Error);
    }

    #[test]
    fn level_warn() {
        assert_eq!(detect_level_in("disk warning threshold"), LogLevel::Warn);
    }

    #[test]
    fn level_unknown() {
        assert_eq!(detect_level_in("nothing special here"), LogLevel::Unknown);
    }

    // ---- parse (integration) ----

    fn make_entry(raw: &str, level: Option<LogLevel>) -> LogEntry {
        LogEntry {
            line_number: 1,
            raw: raw.to_owned(),
            timestamp: None,
            level,
        }
    }

    fn make_log(entries: Vec<LogEntry>) -> LogFile {
        LogFile { path: PathBuf::from("/fake/syslog"), entries, format: None }
    }

    #[test]
    fn parse_sets_timestamp() {
        let raw = "Jan 15 14:23:05 host sshd[123]: session opened for user root";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("Jan 15 14:23:05"));
    }

    #[test]
    fn parse_refines_unknown_level() {
        let raw = "Jan 15 14:23:05 host sshd[123]: error reading config";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Error));
    }

    #[test]
    fn parse_does_not_override_known_level() {
        // Level is already Info — parser must not downgrade it even if the
        // message contains a higher-priority keyword, because the caller set it
        // deliberately.
        let raw = "Jan 15 14:23:05 host sshd[123]: error reading config";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Info))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Info));
    }

    #[test]
    fn parse_skips_non_syslog_line() {
        let raw = "This is not a syslog line at all";
        let mut lf = make_log(vec![make_entry(raw, None)]);
        parse(&mut lf);
        assert!(lf.entries[0].timestamp.is_none());
    }

    #[test]
    fn parse_level_from_message_not_hostname() {
        // Hostname contains "error" — loader would have set Error from the full
        // line, but the parser should use only the message portion.
        // We simulate the loader having set Unknown (as if it somehow didn't
        // fire) to verify the message-scoped detection returns Warn.
        let raw = "Jan 15 14:23:05 error-host myproc[1]: disk space warning";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Warn));
    }
}
