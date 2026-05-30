//! Parser for newline-delimited JSON (NDJSON) log files.
//!
//! Each non-empty line is expected to be a single JSON object.  Fields are
//! extracted with a lightweight hand-written scanner — no `serde_json`, no
//! new dependencies.
//!
//! # Field aliases
//!
//! | Semantic | Accepted key names |
//! |----------|-------------------|
//! | Timestamp | `"timestamp"`, `"time"`, `"ts"`, `"@timestamp"` |
//! | Level     | `"level"`, `"severity"`, `"lvl"` |
//! | Message   | `"message"`, `"msg"` |

use crate::types::{LogFile, LogLevel};

// ---------------------------------------------------------------------------
// Key aliases
// ---------------------------------------------------------------------------

const TS_KEYS:  &[&str] = &["timestamp", "time", "ts", "@timestamp"];
const LVL_KEYS: &[&str] = &["level", "severity", "lvl"];

// ---------------------------------------------------------------------------
// Core scanner
// ---------------------------------------------------------------------------

/// Return the byte offset just past the closing `"` of the JSON string that
/// starts (with its opening `"`) at `s[start]`.
///
/// Handles `\"` escape sequences so we don't terminate early.  Returns `None`
/// if the string is unterminated.
fn end_of_json_string(s: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(s[start], b'"');
    let mut i = start + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2, // skip escaped character
            b'"'  => return Some(i + 1),
            _     => i += 1,
        }
    }
    None // unterminated string
}

/// Search for the JSON key `name` in `line` and return the string value that
/// follows it, as a `&str` slice of `line`.
///
/// Only matches keys that appear in **key position** — i.e. whose opening `"`
/// is immediately preceded (ignoring whitespace) by `{` or `,`.  This prevents
/// false-positive matches when the key name appears inside a value string.
///
/// Returns `None` if the key is absent or the value is not a JSON string.
fn extract_string_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let b = line.as_bytes();
    let key_with_quotes = format!("\"{}\"", name); // e.g. `"timestamp"`
    let kq = key_with_quotes.as_bytes();

    let mut search_from = 0;
    while search_from + kq.len() <= b.len() {
        // Find next occurrence of the quoted key
        let found = b[search_from..]
            .windows(kq.len())
            .position(|w| w == kq)
            .map(|p| p + search_from)?;

        // Verify key position: scan backwards past whitespace; must hit { or ,
        let in_key_position = {
            let mut j = found.saturating_sub(1);
            // skip whitespace
            while j > 0 && (b[j] == b' ' || b[j] == b'\t') {
                j -= 1;
            }
            b[j] == b'{' || b[j] == b','
        };

        if !in_key_position {
            search_from = found + kq.len();
            continue;
        }

        // Advance past the key and the colon
        let after_key = found + kq.len();
        let colon_pos = b[after_key..].iter().position(|&c| c == b':')?;
        let after_colon = after_key + colon_pos + 1;

        // Skip whitespace
        let value_start_offset = b[after_colon..]
            .iter()
            .position(|&c| c != b' ' && c != b'\t')?;
        let value_start = after_colon + value_start_offset;

        // Value must be a string (starts with `"`)
        if b[value_start] != b'"' {
            return None;
        }

        // Find closing quote, honouring escape sequences
        let value_end = end_of_json_string(b, value_start)?;

        // Return the content between the quotes (exclusive)
        return Some(&line[value_start + 1..value_end - 1]);
    }

    None
}

/// Search for the JSON key `name` in `line` and return its value as a `u64`,
/// for the case where the value is a bare JSON number (not quoted).
///
/// Uses the same key-position guard as [`extract_string_field`].  Returns
/// `None` if the key is absent, not in key position, or its value is not a
/// sequence of ASCII digits.
fn extract_number_field(line: &str, name: &str) -> Option<u64> {
    let b = line.as_bytes();
    let key_with_quotes = format!("\"{}\"", name);
    let kq = key_with_quotes.as_bytes();

    let mut search_from = 0;
    while search_from + kq.len() <= b.len() {
        let found = b[search_from..]
            .windows(kq.len())
            .position(|w| w == kq)
            .map(|p| p + search_from)?;

        // Key-position guard (same logic as extract_string_field)
        let in_key_position = {
            let mut j = found.saturating_sub(1);
            while j > 0 && (b[j] == b' ' || b[j] == b'\t') {
                j -= 1;
            }
            b[j] == b'{' || b[j] == b','
        };

        if !in_key_position {
            search_from = found + kq.len();
            continue;
        }

        // Advance past the key and the colon
        let after_key = found + kq.len();
        let colon_pos = b[after_key..].iter().position(|&c| c == b':')?;
        let after_colon = after_key + colon_pos + 1;

        // Skip whitespace
        let value_start_offset = b[after_colon..]
            .iter()
            .position(|&c| c != b' ' && c != b'\t')?;
        let value_start = after_colon + value_start_offset;

        // Value must start with a digit (bare JSON number, not quoted)
        if !b[value_start].is_ascii_digit() {
            return None;
        }

        // Consume consecutive digits
        let digit_len = b[value_start..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();

        let num_str = &line[value_start..value_start + digit_len];
        return num_str.parse::<u64>().ok();
    }

    None
}

// ---------------------------------------------------------------------------
// Level parsing
// ---------------------------------------------------------------------------

/// Map a JSON level string (e.g. `"error"`, `"WARN"`, `"30"`) to [`LogLevel`].
///
/// Uses the same keyword-priority order as `loader::detect_level`:
/// error > warn > info > debug > trace > unknown.
///
/// Numeric level conventions (common in Bunyan/Pino) are also supported:
/// 60 → Error, 50 → Warn, 40 → Info, 30 → Debug, 10–20 → Trace.
fn level_from_str(s: &str) -> LogLevel {
    // --- numeric (Bunyan/Pino) ---
    if let Ok(n) = s.trim().parse::<u32>() {
        return match n {
            60..=u32::MAX => LogLevel::Error,
            50..=59       => LogLevel::Warn,
            40..=49       => LogLevel::Info,
            30..=39       => LogLevel::Debug,
            _             => LogLevel::Trace,
        };
    }

    // --- keyword (case-insensitive) ---
    let lower = s.to_lowercase();
    if lower.contains("error") || lower.contains("fatal") || lower.contains("crit") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else if lower.contains("info") {
        LogLevel::Info
    } else if lower.contains("debug") {
        LogLevel::Debug
    } else if lower.contains("trace") || lower.contains("verbose") {
        LogLevel::Trace
    } else {
        LogLevel::Unknown
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse a newline-delimited JSON [`LogFile`] in place.
///
/// For each non-empty entry:
/// - `entry.timestamp` is set from the first matching timestamp field alias
///   (`"timestamp"`, `"time"`, `"ts"`, `"@timestamp"`).
/// - `entry.level` is set by extracting the first matching level field alias
///   (`"level"`, `"severity"`, `"lvl"`) and mapping it through
///   [`level_from_str`].
/// - `entry.raw` is never modified.
///
/// Lines that do not start with `{` (e.g. blank lines, JSON array brackets)
/// are silently skipped.
pub fn parse(log_file: &mut LogFile) {
    for entry in &mut log_file.entries {
        let line = entry.raw.trim();
        if !line.starts_with('{') {
            continue;
        }

        // 1. Timestamp — try aliases in order
        if entry.timestamp.is_none() {
            for &key in TS_KEYS {
                if let Some(val) = extract_string_field(line, key) {
                    entry.timestamp = Some(val.to_owned());
                    break;
                }
            }
        }

        // 2. Level — try aliases in order; always set (overwrites Unknown)
        let needs_level = matches!(&entry.level, None | Some(LogLevel::Unknown));
        if needs_level {
            for &key in LVL_KEYS {
                // Try string value first (e.g. "level":"warn")
                if let Some(val) = extract_string_field(line, key) {
                    entry.level = Some(level_from_str(val));
                    break;
                }
                // Fall back to bare number (e.g. "level":50 — Bunyan/Pino)
                if let Some(n) = extract_number_field(line, key) {
                    entry.level = Some(level_from_str(&n.to_string()));
                    break;
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
    
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return record;
    }
    
    for &key in TS_KEYS {
        if let Some(val) = extract_string_field(trimmed, key) {
            let offset = val.as_ptr() as usize - line.as_ptr() as usize;
            record.ts_start = offset as u16;
            record.ts_len = val.len() as u8;
            break;
        }
    }
    
    for &key in LVL_KEYS {
        if let Some(val) = extract_string_field(trimmed, key) {
            record.level = level_from_str(val);
            break;
        }
        if let Some(n) = extract_number_field(trimmed, key) {
            record.level = level_from_str(&n.to_string());
            break;
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

    // ---- extract_string_field ----

    #[test]
    fn extracts_simple_field() {
        let line = r#"{"timestamp":"2024-01-15T14:23:05Z","level":"error"}"#;
        assert_eq!(
            extract_string_field(line, "timestamp"),
            Some("2024-01-15T14:23:05Z")
        );
    }

    #[test]
    fn extracts_second_field() {
        let line = r#"{"timestamp":"2024-01-15T14:23:05Z","level":"error"}"#;
        assert_eq!(extract_string_field(line, "level"), Some("error"));
    }

    #[test]
    fn rejects_key_inside_value() {
        // "level" appears inside the "message" value — must not match as a key
        let line = r#"{"message":"the level was high","level":"warn"}"#;
        assert_eq!(extract_string_field(line, "level"), Some("warn"));
    }

    #[test]
    fn handles_escaped_quote_in_value() {
        let line = r#"{"msg":"say \"hello\"","level":"info"}"#;
        assert_eq!(extract_string_field(line, "msg"), Some(r#"say \"hello\""#));
        assert_eq!(extract_string_field(line, "level"), Some("info"));
    }

    #[test]
    fn returns_none_for_absent_key() {
        let line = r#"{"message":"hi"}"#;
        assert_eq!(extract_string_field(line, "level"), None);
    }

    #[test]
    fn extracts_at_timestamp_alias() {
        let line = r#"{"@timestamp":"2024-01-15T14:23:05.000Z","severity":"WARN"}"#;
        assert_eq!(
            extract_string_field(line, "@timestamp"),
            Some("2024-01-15T14:23:05.000Z")
        );
    }

    // ---- level_from_str ----

    #[test]
    fn level_error_keywords() {
        for s in &["error", "ERROR", "Error", "fatal", "CRITICAL", "crit"] {
            assert_eq!(level_from_str(s), LogLevel::Error, "input: {s}");
        }
    }

    #[test]
    fn level_warn_keywords() {
        for s in &["warn", "WARN", "warning", "WARNING"] {
            assert_eq!(level_from_str(s), LogLevel::Warn, "input: {s}");
        }
    }

    #[test]
    fn level_info_keywords() {
        for s in &["info", "INFO", "information"] {
            assert_eq!(level_from_str(s), LogLevel::Info, "input: {s}");
        }
    }

    #[test]
    fn level_debug_keywords() {
        assert_eq!(level_from_str("debug"), LogLevel::Debug);
        assert_eq!(level_from_str("DEBUG"), LogLevel::Debug);
    }

    #[test]
    fn level_trace_keywords() {
        assert_eq!(level_from_str("trace"),   LogLevel::Trace);
        assert_eq!(level_from_str("verbose"), LogLevel::Trace);
    }

    #[test]
    fn level_numeric_bunyan() {
        assert_eq!(level_from_str("60"), LogLevel::Error);
        assert_eq!(level_from_str("50"), LogLevel::Warn);
        assert_eq!(level_from_str("40"), LogLevel::Info);
        assert_eq!(level_from_str("30"), LogLevel::Debug);
        assert_eq!(level_from_str("20"), LogLevel::Trace);
        assert_eq!(level_from_str("10"), LogLevel::Trace);
    }

    #[test]
    fn level_unknown_fallback() {
        assert_eq!(level_from_str("notice"), LogLevel::Unknown);
        assert_eq!(level_from_str(""),       LogLevel::Unknown);
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
        LogFile {
            path: std::path::PathBuf::from("/fake/app.json"),
            entries,
            format: None,
        }
    }

    #[test]
    fn parse_standard_fields() {
        let raw = r#"{"timestamp":"2024-01-15T14:23:05Z","level":"error","message":"oops"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("2024-01-15T14:23:05Z"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Error));
    }

    #[test]
    fn parse_alias_time_and_severity() {
        let raw = r#"{"time":"2024-06-01T00:00:00Z","severity":"WARN","msg":"low disk"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("2024-06-01T00:00:00Z"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Warn));
    }

    #[test]
    fn parse_alias_ts_and_lvl() {
        let raw = r#"{"ts":"2024-06-01T12:00:00Z","lvl":"debug"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("2024-06-01T12:00:00Z"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Debug));
    }

    #[test]
    fn parse_at_timestamp_alias() {
        let raw = r#"{"@timestamp":"2024-06-01T08:00:00.000Z","level":"info"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("2024-06-01T08:00:00.000Z"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Info));
    }

    #[test]
    fn parse_bunyan_numeric_level() {
        let raw = r#"{"time":"2024-01-01T00:00:00Z","level":50,"msg":"test"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].timestamp.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(lf.entries[0].level, Some(LogLevel::Warn));
    }

    #[test]
    fn parse_does_not_override_definite_level() {
        let raw = r#"{"timestamp":"2024-01-15T14:23:05Z","level":"error"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Info))]);
        parse(&mut lf);
        // Info is definite — should not be overwritten by Error from JSON
        assert_eq!(lf.entries[0].level, Some(LogLevel::Info));
    }

    #[test]
    fn parse_skips_non_object_lines() {
        let entries = vec![
            make_entry("[",          None),  // array bracket
            make_entry("",           None),  // blank
            make_entry("   ",        None),  // whitespace only
        ];
        let mut lf = make_log(entries);
        parse(&mut lf);
        for e in &lf.entries {
            assert!(e.timestamp.is_none());
        }
    }

    #[test]
    fn parse_key_inside_value_does_not_pollute() {
        // "message" value contains the word "level" — must not affect level extraction
        let raw = r#"{"message":"level was exceeded","level":"warn"}"#;
        let mut lf = make_log(vec![make_entry(raw, Some(LogLevel::Unknown))]);
        parse(&mut lf);
        assert_eq!(lf.entries[0].level, Some(LogLevel::Warn));
    }

    #[test]
    fn parse_multiple_ndjson_lines() {
        let lines = vec![
            make_entry(
                r#"{"timestamp":"2024-01-01T00:00:00Z","level":"info","msg":"start"}"#,
                Some(LogLevel::Unknown),
            ),
            make_entry(
                r#"{"timestamp":"2024-01-01T00:00:01Z","level":"error","msg":"crash"}"#,
                Some(LogLevel::Unknown),
            ),
        ];
        let mut lf = make_log(lines);
        parse(&mut lf);

        assert_eq!(lf.entries[0].level, Some(LogLevel::Info));
        assert_eq!(lf.entries[1].level, Some(LogLevel::Error));
        assert_eq!(
            lf.entries[1].timestamp.as_deref(),
            Some("2024-01-01T00:00:01Z")
        );
    }
}
