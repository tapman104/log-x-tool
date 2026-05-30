//! Parser for Android logcat threadtime-format lines.
//!
//! Expected line structure:
//! ```text
//! MM-DD HH:MM:SS.mmm  PID  TID LEVEL TAG: message
//! ```
//!
//! Lines beginning with `--------- beginning of` are logcat section banners;
//! they carry no timestamp or level and are left untouched.

use crate::types::{LogFile, LogLevel};

// ---------------------------------------------------------------------------
// Timestamp extraction  —  MM-DD HH:MM:SS.mmm
// ---------------------------------------------------------------------------

/// Try to parse the leading `MM-DD HH:MM:SS.mmm` timestamp from `s`.
///
/// Validates every digit and separator position directly on bytes.
/// Returns a slice of `s` covering the token, or `None` on mismatch.
///
/// Exposed as `pub(super)` so `mod.rs` can reuse it for format detection.
pub(super) fn try_parse_timestamp(s: &str) -> Option<&str> {
    // "MM-DD HH:MM:SS.mmm" = 18 chars minimum
    if s.len() < 18 {
        return None;
    }

    let b = s.as_bytes();

    // MM-DD
    if !b[0].is_ascii_digit() || !b[1].is_ascii_digit() { return None; }
    if b[2] != b'-'                                      { return None; }
    if !b[3].is_ascii_digit() || !b[4].is_ascii_digit() { return None; }

    // space between date and time
    if b[5] != b' ' { return None; }

    // HH:MM:SS
    if !b[6].is_ascii_digit()  || !b[7].is_ascii_digit()  { return None; }
    if b[8] != b':'                                         { return None; }
    if !b[9].is_ascii_digit()  || !b[10].is_ascii_digit() { return None; }
    if b[11] != b':'                                        { return None; }
    if !b[12].is_ascii_digit() || !b[13].is_ascii_digit() { return None; }

    // decimal point + millis
    if b[14] != b'.'                                        { return None; }
    if !b[15].is_ascii_digit() || !b[16].is_ascii_digit() || !b[17].is_ascii_digit() {
        return None;
    }

    // Must be followed by whitespace or end-of-string
    if s.len() > 18 && !b[18].is_ascii_whitespace() {
        return None;
    }

    Some(&s[..18])
}

// ---------------------------------------------------------------------------
// Level parsing
// ---------------------------------------------------------------------------

/// Map a logcat single-character level to [`LogLevel`].
///
/// Returns `None` for any character that is not a recognised logcat level letter.
fn map_level(ch: u8) -> Option<LogLevel> {
    match ch {
        b'V' => Some(LogLevel::Trace),
        b'D' => Some(LogLevel::Debug),
        b'I' => Some(LogLevel::Info),
        b'W' => Some(LogLevel::Warn),
        b'E' | b'F' => Some(LogLevel::Error),
        _ => None,
    }
}

/// Find the logcat level character in the fields that follow the timestamp.
///
/// Threadtime layout after the timestamp:
/// ```text
/// <whitespace> PID <whitespace> TID <whitespace> LEVEL <whitespace> TAG: msg
/// ```
/// The level is the third whitespace-separated token after the timestamp and
/// is always a single character.  We scan tokens and return the first one that
/// maps to a known level.
fn parse_level_from_fields(after_timestamp: &str) -> Option<LogLevel> {
    // Token index after the timestamp:
    //   0 → PID   (numeric)
    //   1 → TID   (numeric)
    //   2 → LEVEL (single char)
    for (i, token) in after_timestamp.split_whitespace().enumerate() {
        if i == 2 {
            // LEVEL field — must be exactly one char
            if token.len() == 1 {
                return map_level(token.as_bytes()[0]);
            }
            // Malformed; stop searching.
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse a logcat-format [`LogFile`] in place.
///
/// For each entry:
/// - Lines starting with `--------- beginning` are banner lines; they are
///   skipped (no timestamp or level to parse).
/// - For all other lines the leading `MM-DD HH:MM:SS.mmm` token is extracted
///   and stored in `entry.timestamp`.
/// - `entry.level` is **always** overridden with the value parsed from the
///   logcat level field (`V/D/I/W/E/F`) because the logcat level is
///   authoritative and already precise.
/// - `entry.raw` is never modified.
pub fn parse(log_file: &mut LogFile) {
    for entry in &mut log_file.entries {
        let line = entry.raw.trim_start();

        // Skip logcat banner lines
        if line.starts_with("--------- beginning") {
            continue;
        }

        if let Some(ts) = try_parse_timestamp(line) {
            // 1. Timestamp
            entry.timestamp = Some(ts.to_owned());

            // 2. Level (authoritative — always overwrite)
            let after_ts = &line[ts.len()..];
            if let Some(level) = parse_level_from_fields(after_ts) {
                entry.level = Some(level);
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

    let trimmed = line.trim_start();
    if trimmed.starts_with("--------- beginning") {
        return record;
    }

    if let Some(ts) = try_parse_timestamp(trimmed) {
        let offset = ts.as_ptr() as usize - line.as_ptr() as usize;
        record.ts_start = offset as u16;
        record.ts_len = ts.len() as u8;

        let after_ts = &trimmed[ts.len()..];
        if let Some(level) = parse_level_from_fields(after_ts) {
            record.level = level;
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

    // ---- try_parse_timestamp ----

    #[test]
    fn timestamp_basic() {
        assert_eq!(
            try_parse_timestamp("05-28 14:23:05.123 1234 5678 I MyTag: message"),
            Some("05-28 14:23:05.123")
        );
    }

    #[test]
    fn timestamp_at_string_end() {
        assert_eq!(
            try_parse_timestamp("05-28 14:23:05.123"),
            Some("05-28 14:23:05.123")
        );
    }

    #[test]
    fn timestamp_rejects_wrong_separator() {
        // '/' instead of '-'
        assert_eq!(try_parse_timestamp("05/28 14:23:05.123 rest"), None);
    }

    #[test]
    fn timestamp_rejects_syslog_shape() {
        // Syslog "Jan 15 14:23:05" must not be accepted
        assert_eq!(try_parse_timestamp("Jan 15 14:23:05 host p: msg"), None);
    }

    #[test]
    fn timestamp_rejects_too_short() {
        assert_eq!(try_parse_timestamp("05-28 14:23"), None);
    }

    #[test]
    fn timestamp_rejects_non_digit_in_millis() {
        assert_eq!(try_parse_timestamp("05-28 14:23:05.1X3 rest"), None);
    }

    // ---- map_level ----

    #[test]
    fn level_mapping() {
        assert_eq!(map_level(b'V'), Some(LogLevel::Trace));
        assert_eq!(map_level(b'D'), Some(LogLevel::Debug));
        assert_eq!(map_level(b'I'), Some(LogLevel::Info));
        assert_eq!(map_level(b'W'), Some(LogLevel::Warn));
        assert_eq!(map_level(b'E'), Some(LogLevel::Error));
        assert_eq!(map_level(b'F'), Some(LogLevel::Error));
        assert_eq!(map_level(b'X'), None);
    }

    // ---- parse_level_from_fields ----

    #[test]
    fn level_from_fields_info() {
        //  PID  TID LEVEL
        assert_eq!(
            parse_level_from_fields("  1234  5678 I MyTag: msg"),
            Some(LogLevel::Info)
        );
    }

    #[test]
    fn level_from_fields_error() {
        assert_eq!(
            parse_level_from_fields("  1234  5678 E Crash: oops"),
            Some(LogLevel::Error)
        );
    }

    #[test]
    fn level_from_fields_fatal_maps_to_error() {
        assert_eq!(
            parse_level_from_fields("  1234  5678 F FATAL: abort"),
            Some(LogLevel::Error)
        );
    }

    #[test]
    fn level_from_fields_unknown_char() {
        assert_eq!(
            parse_level_from_fields("  1234  5678 X UnknownTag: msg"),
            None
        );
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
        LogFile { path: std::path::PathBuf::from("/fake/logcat.log"), entries, format: None }
    }

    #[test]
    fn parse_sets_timestamp_and_level() {
        let raw = "05-28 14:23:05.123  1234  5678 W MyTag: low memory";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("05-28 14:23:05.123"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Warn));
    }

    #[test]
    fn parse_overrides_loader_level() {
        // Loader may have set Info; logcat level field says Error — Error wins.
        let raw = "05-28 14:23:05.123  1234  5678 E SomeTag: crash";
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Info))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Error));
    }

    #[test]
    fn parse_verbose_maps_to_trace() {
        let raw = "05-28 14:23:05.456  1111  2222 V VerboseTag: lots of output";
        let mut lf = make_log(vec![make_entry(raw, None)]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Trace));
    }

    #[test]
    fn parse_skips_banner_line() {
        let raw = "--------- beginning of /dev/log/main";
        let mut lf = make_log(vec![make_entry(raw, None)]);
        parse(&mut lf);
        assert!(lf.entries[0].timestamp.is_none());
        assert!(lf.entries[0].level.is_none());
    }

    #[test]
    fn parse_skips_non_logcat_line() {
        let raw = "not a logcat line";
        let mut lf = make_log(vec![make_entry(raw, None)]);
        parse(&mut lf);
        assert!(lf.entries[0].timestamp.is_none());
    }

    #[test]
    fn parse_mixed_entries() {
        let banner  = "--------- beginning of /dev/log/main";
        let info    = "05-28 10:00:00.000   100   200 I Tag: started";
        let err_ln  = "05-28 10:00:01.500   100   200 E Tag: failed";
        let mut lf = make_log(vec![
            make_entry(banner, None),
            make_entry(info,   Some(LogLevel::Unknown)),
            make_entry(err_ln, Some(LogLevel::Unknown)),
        ]);
        parse(&mut lf);

        // banner untouched
        assert!(lf.entries[0].timestamp.is_none());

        // info line
        assert_eq!(lf.entries[1].timestamp.as_deref(), Some("05-28 10:00:00.000"));
        assert_eq!(lf.entries[1].level, Some(LogLevel::Info));

        // error line
        assert_eq!(lf.entries[2].timestamp.as_deref(), Some("05-28 10:00:01.500"));
        assert_eq!(lf.entries[2].level, Some(LogLevel::Error));
    }
}
