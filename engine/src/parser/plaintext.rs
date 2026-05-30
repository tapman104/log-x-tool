use crate::types::LogFile;

// ---------------------------------------------------------------------------
// Helper predicates
// ---------------------------------------------------------------------------

/// True when every byte in `s` is an ASCII decimal digit.
#[inline]
fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// True when `s` has exactly `n` characters and they are all ASCII digits.
#[inline]
fn digits_of_len(s: &str, n: usize) -> bool {
    s.len() == n && all_digits(s)
}

// ---------------------------------------------------------------------------
// Format 1 – ISO 8601 with T and optional millis / timezone suffix
//
// Examples accepted:
//   2024-01-15T14:23:05Z
//   2024-01-15T14:23:05.123Z
//   2024-01-15T14:23:05+05:30
//   2024-01-15T14:23:05
//
// Minimum required: YYYY-MM-DDTHH:MM:SS  (19 chars)
// After the seconds the remainder is optional (millis, Z, ±HH:MM, etc.)
// We return a slice up to but not including the first space / non-timestamp char
// that follows the optional suffix.
// ---------------------------------------------------------------------------
fn try_parse_iso_t(s: &str) -> Option<&str> {
    // Need at least "YYYY-MM-DDTHH:MM:SS" = 19 chars
    if s.len() < 19 {
        return None;
    }

    let b = s.as_bytes();

    // YYYY-MM-DD
    if !digits_of_len(&s[0..4], 4) { return None; }
    if b[4] != b'-' { return None; }
    if !digits_of_len(&s[5..7], 2) { return None; }
    if b[7] != b'-' { return None; }
    if !digits_of_len(&s[8..10], 2) { return None; }

    // T separator
    if b[10] != b'T' { return None; }

    // HH:MM:SS
    if !digits_of_len(&s[11..13], 2) { return None; }
    if b[13] != b':' { return None; }
    if !digits_of_len(&s[14..16], 2) { return None; }
    if b[16] != b':' { return None; }
    if !digits_of_len(&s[17..19], 2) { return None; }

    // Everything up to here is valid; now consume the optional suffix so we
    // return the full timestamp token.
    let mut end = 19;
    let rest = &s[end..];

    // Optional fractional seconds: .ddd…
    if rest.starts_with('.') {
        end += 1; // consume '.'
        let frac_start = end;
        while end < s.len() && s.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
        if end == frac_start {
            // '.' with no digits is not a valid suffix — back up
            end -= 1;
        }
    }

    // Optional timezone: Z  or  ±HH:MM
    if end < s.len() {
        match s.as_bytes()[end] {
            b'Z' => { end += 1; }
            b'+' | b'-' => {
                // ±HH:MM = 6 chars
                if end + 6 <= s.len()
                    && digits_of_len(&s[end + 1..end + 3], 2)
                    && s.as_bytes()[end + 3] == b':'
                    && digits_of_len(&s[end + 4..end + 6], 2)
                {
                    end += 6;
                }
            }
            _ => {}
        }
    }

    // The character immediately after the timestamp must be a space, end-of-string,
    // or a non-alphanumeric separator — reject if we're in the middle of a word.
    if end < s.len() && s.as_bytes()[end].is_ascii_alphanumeric() {
        return None;
    }

    Some(&s[..end])
}

// ---------------------------------------------------------------------------
// Format 2 – ISO 8601 space-separated (no T)
//
// Examples: "2024-01-15 14:23:05"  "2024-01-15 14:23:05.456"
//
// Exactly like format 1 but position 10 is a space instead of 'T', and there
// is no timezone suffix (the space is the separator between date and time, so
// any further suffix would need another separator that we don't consume here).
// ---------------------------------------------------------------------------
fn try_parse_iso_space(s: &str) -> Option<&str> {
    if s.len() < 19 {
        return None;
    }

    let b = s.as_bytes();

    // YYYY-MM-DD
    if !digits_of_len(&s[0..4], 4) { return None; }
    if b[4] != b'-' { return None; }
    if !digits_of_len(&s[5..7], 2) { return None; }
    if b[7] != b'-' { return None; }
    if !digits_of_len(&s[8..10], 2) { return None; }

    // Space separator between date and time
    if b[10] != b' ' { return None; }

    // HH:MM:SS
    if !digits_of_len(&s[11..13], 2) { return None; }
    if b[13] != b':' { return None; }
    if !digits_of_len(&s[14..16], 2) { return None; }
    if b[16] != b':' { return None; }
    if !digits_of_len(&s[17..19], 2) { return None; }

    let mut end = 19;

    // Optional fractional seconds
    if end < s.len() && s.as_bytes()[end] == b'.' {
        end += 1;
        let frac_start = end;
        while end < s.len() && s.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
        if end == frac_start {
            end -= 1; // back up the '.'
        }
    }

    // Must be followed by space, end-of-string, or non-alphanumeric
    if end < s.len() && s.as_bytes()[end].is_ascii_alphanumeric() {
        return None;
    }

    Some(&s[..end])
}

// ---------------------------------------------------------------------------
// Format 3 – Syslog month-name style
//
// Examples: "Jan 15 14:23:05"   "Jan  5 14:23:05"  (single-digit day padded
//                                                     with a space in real syslog)
// Layout:   MMM SP (SP)DD SP HH:MM:SS
//           0-2  3  4   5-6  7  8-9 10 11-12 13 14-15
//
// We accept both " 5" (space-padded) and "15" for the day field.
// ---------------------------------------------------------------------------
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn try_parse_syslog(s: &str) -> Option<&str> {
    // Minimum: "Jan  5 14:23:05" = 15 chars
    if s.len() < 15 {
        return None;
    }

    // First 3 chars must be a known month abbreviation
    if !MONTHS.contains(&&s[..3]) {
        return None;
    }

    let b = s.as_bytes();

    // Position 3 must be a space
    if b[3] != b' ' { return None; }

    // Position 4: either a space (single-digit day) or a digit
    let day_start;
    if b[4] == b' ' {
        // space-padded single digit: " D"
        day_start = 5;
    } else if b[4].is_ascii_digit() {
        day_start = 4;
    } else {
        return None;
    }

    // Day: 1 or 2 digits at day_start
    let day_end;
    if day_start + 1 < s.len() && s.as_bytes()[day_start + 1].is_ascii_digit() {
        day_end = day_start + 2;
    } else if s.as_bytes()[day_start].is_ascii_digit() {
        day_end = day_start + 1;
    } else {
        return None;
    }

    // Space between day and time
    if day_end >= s.len() || b[day_end] != b' ' { return None; }

    let t = day_end + 1; // start of HH:MM:SS

    // Need at least 8 more chars: "HH:MM:SS"
    if t + 8 > s.len() { return None; }

    if !digits_of_len(&s[t..t + 2], 2) { return None; }
    if b[t + 2] != b':' { return None; }
    if !digits_of_len(&s[t + 3..t + 5], 2) { return None; }
    if b[t + 5] != b':' { return None; }
    if !digits_of_len(&s[t + 6..t + 8], 2) { return None; }

    let end = t + 8;

    // Must be followed by space, end-of-string, or non-alphanumeric
    if end < s.len() && s.as_bytes()[end].is_ascii_alphanumeric() {
        return None;
    }

    Some(&s[..end])
}

// ---------------------------------------------------------------------------
// Format 4 – Time only
//
// Examples: "14:23:05"   "14:23:05.999"
// ---------------------------------------------------------------------------
fn try_parse_time_only(s: &str) -> Option<&str> {
    // Minimum: "HH:MM:SS" = 8 chars
    if s.len() < 8 {
        return None;
    }

    let b = s.as_bytes();

    if !digits_of_len(&s[0..2], 2) { return None; }
    if b[2] != b':' { return None; }
    if !digits_of_len(&s[3..5], 2) { return None; }
    if b[5] != b':' { return None; }
    if !digits_of_len(&s[6..8], 2) { return None; }

    let mut end = 8;

    // Optional fractional seconds
    if end < s.len() && s.as_bytes()[end] == b'.' {
        end += 1;
        let frac_start = end;
        while end < s.len() && s.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
        if end == frac_start {
            end -= 1;
        }
    }

    // Must be followed by space, end-of-string, or non-alphanumeric
    if end < s.len() && s.as_bytes()[end].is_ascii_alphanumeric() {
        return None;
    }

    Some(&s[..end])
}

// ---------------------------------------------------------------------------
// Top-level extraction
// ---------------------------------------------------------------------------

/// Try every known format in priority order and return the matched slice, or
/// `None` if the line carries no recognisable timestamp.
fn extract_timestamp(line: &str) -> Option<&str> {
    let s = line.trim_start();
    try_parse_iso_t(s)
        .or_else(|| try_parse_iso_space(s))
        .or_else(|| try_parse_syslog(s))
        .or_else(|| try_parse_time_only(s))
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse a plain-text log file in place.
///
/// For each [`LogEntry`] whose `timestamp` is currently `None`, attempts to
/// extract a timestamp from the start of `raw` using four recognised formats
/// (tried in order, most specific first):
///
/// 1. ISO 8601 with `T` and optional millis/timezone  
///    e.g. `2024-01-15T14:23:05.123Z`
/// 2. ISO 8601 space-separated  
///    e.g. `2024-01-15 14:23:05`
/// 3. Syslog month-name style  
///    e.g. `Jan 15 14:23:05`
/// 4. Time only  
///    e.g. `14:23:05`
///
/// Entries for which no format matches are left with `timestamp = None`.
///
/// [`LogEntry`]: crate::types::LogEntry
pub fn parse(log_file: &mut LogFile) {
    for entry in &mut log_file.entries {
        if entry.timestamp.is_none() {
            entry.timestamp = extract_timestamp(&entry.raw).map(str::to_owned);
        }
    }
}

pub(super) fn parse_line(line: &str) -> crate::types::LineRecord {
    let mut record = crate::types::LineRecord {
        level: crate::types::LogLevel::Unknown,
        ts_start: 0,
        ts_len: 0,
    };

    if let Some(ts) = extract_timestamp(line) {
        let offset = ts.as_ptr() as usize - line.as_ptr() as usize;
        record.ts_start = offset as u16;
        record.ts_len = ts.len() as u8;
    }

    // plain text log level should be extracted from line if possible, similar to loader::detect_level.
    let lower = line.to_lowercase();
    if lower.contains("error") {
        record.level = crate::types::LogLevel::Error;
    } else if lower.contains("warn") {
        record.level = crate::types::LogLevel::Warn;
    } else if lower.contains("info") {
        record.level = crate::types::LogLevel::Info;
    } else if lower.contains("debug") {
        record.level = crate::types::LogLevel::Debug;
    } else if lower.contains("trace") {
        record.level = crate::types::LogLevel::Trace;
    }

    record
}

// ---------------------------------------------------------------------------
// Unit tests (run with `cargo test -p engine`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- try_parse_iso_t ---

    #[test]
    fn iso_t_basic() {
        assert_eq!(
            try_parse_iso_t("2024-01-15T14:23:05Z rest"),
            Some("2024-01-15T14:23:05Z")
        );
    }

    #[test]
    fn iso_t_millis() {
        assert_eq!(
            try_parse_iso_t("2024-01-15T14:23:05.123Z message"),
            Some("2024-01-15T14:23:05.123Z")
        );
    }

    #[test]
    fn iso_t_offset() {
        assert_eq!(
            try_parse_iso_t("2024-01-15T14:23:05+05:30 msg"),
            Some("2024-01-15T14:23:05+05:30")
        );
    }

    #[test]
    fn iso_t_no_suffix() {
        assert_eq!(
            try_parse_iso_t("2024-01-15T14:23:05 msg"),
            Some("2024-01-15T14:23:05")
        );
    }

    #[test]
    fn iso_t_rejects_space_sep() {
        assert_eq!(try_parse_iso_t("2024-01-15 14:23:05"), None);
    }

    // --- try_parse_iso_space ---

    #[test]
    fn iso_space_basic() {
        assert_eq!(
            try_parse_iso_space("2024-01-15 14:23:05 msg"),
            Some("2024-01-15 14:23:05")
        );
    }

    #[test]
    fn iso_space_millis() {
        assert_eq!(
            try_parse_iso_space("2024-01-15 14:23:05.456 msg"),
            Some("2024-01-15 14:23:05.456")
        );
    }

    #[test]
    fn iso_space_rejects_t_sep() {
        assert_eq!(try_parse_iso_space("2024-01-15T14:23:05"), None);
    }

    // --- try_parse_syslog ---

    #[test]
    fn syslog_two_digit_day() {
        assert_eq!(
            try_parse_syslog("Jan 15 14:23:05 host proc: msg"),
            Some("Jan 15 14:23:05")
        );
    }

    #[test]
    fn syslog_space_padded_day() {
        assert_eq!(
            try_parse_syslog("Jan  5 14:23:05 host proc: msg"),
            Some("Jan  5 14:23:05")
        );
    }

    #[test]
    fn syslog_rejects_unknown_month() {
        assert_eq!(try_parse_syslog("Xyz 15 14:23:05"), None);
    }

    // --- try_parse_time_only ---

    #[test]
    fn time_only_basic() {
        assert_eq!(
            try_parse_time_only("14:23:05 msg"),
            Some("14:23:05")
        );
    }

    #[test]
    fn time_only_millis() {
        assert_eq!(
            try_parse_time_only("14:23:05.999 msg"),
            Some("14:23:05.999")
        );
    }

    #[test]
    fn time_only_rejects_non_digit() {
        assert_eq!(try_parse_time_only("xx:23:05"), None);
    }

    // --- priority ordering via extract_timestamp ---

    #[test]
    fn priority_iso_t_over_time_only() {
        // "14:23:05" appears inside an ISO-T string; iso_t should win
        let ts = extract_timestamp("2024-01-15T14:23:05Z msg");
        assert_eq!(ts, Some("2024-01-15T14:23:05Z"));
    }

    #[test]
    fn no_timestamp_returns_none() {
        assert_eq!(extract_timestamp("no timestamp here at all"), None);
    }

    #[test]
    fn leading_whitespace_is_trimmed() {
        assert_eq!(
            extract_timestamp("  14:23:05 msg"),
            Some("14:23:05")
        );
    }
}
