//! Log-format detection and dispatch.
//!
//! [`parse_file`] is the single public entry point.  It sniffs the file format
//! from the path extension and the first non-empty line of already-loaded
//! content, then delegates to the appropriate sub-module.

mod json;
mod logcat;
mod plaintext;
mod syslog;

use crate::types::LogFile;

/// Formats the engine knows about (or will know about).
#[derive(Debug, PartialEq)]
enum LogFormat {
    PlainText,
    /// Android logcat output (`tag  PID  TID  level  msg`).
    Logcat,
    /// BSD/Linux syslog (`MMM DD HH:MM:SS host proc[pid]: msg`).
    Syslog,
    /// Newline-delimited JSON or a JSON array of log objects.
    Json,
}

/// Detect the log format from the file extension and/or first-line content.
///
/// Extension has priority; first-line sniffing is the fallback for files
/// without a recognised extension (or no extension at all).
fn detect_format(log_file: &LogFile) -> LogFormat {
    // --- extension sniff ---
    if let Some(ext) = log_file.path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "json" | "jsonl" | "ndjson" => return LogFormat::Json,
            "log" | "txt" | "out" => {} // fall through to content sniff
            _ => {}
        }
    }

    // --- first-line content sniff ---
    if let Some(first) = log_file.entries.first() {
        let line = first.raw.trim_start();

        // Logcat threadtime format: "MM-DD HH:MM:SS.mmm PID TID LEVEL TAG: msg"
        // Validate the full timestamp shape so we don't false-positive on
        // unrelated MM-DD patterns.  The banner line ("--------- beginning …")
        // is also a reliable logcat signal for files whose first real entry
        // is preceded by a section header.
        if line.starts_with("--------- beginning")
            || logcat::try_parse_timestamp(line).is_some()
        {
            return LogFormat::Logcat;
        }

        // Syslog: validate the full "MMM (D)D HH:MM:SS" shape rather than
        // just the month prefix, so that ordinary English sentences starting
        // with a month name (e.g. "January report…") are not misclassified.
        // Content sniff intentionally wins over .log / .txt extension so that
        // syslog files stored with a generic extension are still detected.
        if syslog::try_parse_timestamp(line).is_some() {
            return LogFormat::Syslog;
        }

        // JSON: first non-whitespace character is '{' or '['
        if line.starts_with('{') || line.starts_with('[') {
            return LogFormat::Json;
        }
    }

    LogFormat::PlainText
}

/// Parse `log_file` in place, filling in timestamps and any other fields the
/// loader left empty.
///
/// # Format detection
/// The format is detected automatically; callers do not need to specify it.
/// Detection order:
/// 1. File extension (`.json`, `.jsonl`, `.ndjson`, `.log`, `.txt`, `.out`)
/// 2. First-line content sniff (logcat header, syslog month prefix, JSON brace)
/// 3. Falls back to plain-text when nothing else matches.
///
/// After detection the human-readable format name is stored in
/// [`LogFile::format`] so the UI can display it without re-running detection.
pub fn parse_file(log_file: &mut LogFile) {
    let fmt = detect_format(log_file);

    // Record the format name before dispatching (while we still own `fmt`).
    log_file.format = Some(
        match fmt {
            LogFormat::PlainText => "Plain text",
            LogFormat::Logcat    => "Logcat",
            LogFormat::Syslog    => "Syslog",
            LogFormat::Json      => "JSON",
        }
        .to_owned(),
    );

    match fmt {
        LogFormat::PlainText => plaintext::parse(log_file),
        LogFormat::Logcat    => logcat::parse(log_file),
        LogFormat::Syslog    => syslog::parse(log_file),
        LogFormat::Json      => json::parse(log_file),
    }
}

// ---------------------------------------------------------------------------
// Streaming parsing (Phase 2)
// ---------------------------------------------------------------------------

use crate::types::{LineIndex, ParsedIndex, LineRecord};
use rayon::prelude::*;

fn detect_format_index(index: &LineIndex) -> LogFormat {
    if let Some(ext) = index.path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "json" | "jsonl" | "ndjson" => return LogFormat::Json,
            "log" | "txt" | "out" => {} // fall through to content sniff
            _ => {}
        }
    }

    if index.len() > 0 {
        let line = index.line_str(0).trim_start();
        if line.starts_with("--------- beginning")
            || logcat::try_parse_timestamp(line).is_some()
        {
            return LogFormat::Logcat;
        }

        if syslog::try_parse_timestamp(line).is_some() {
            return LogFormat::Syslog;
        }

        if line.starts_with('{') || line.starts_with('[') {
            return LogFormat::Json;
        }
    }

    LogFormat::PlainText
}

pub fn parse_index(mut index: LineIndex) -> ParsedIndex {
    let fmt = detect_format_index(&index);

    let format_name = match fmt {
        LogFormat::PlainText => "Plain text",
        LogFormat::Logcat    => "Logcat",
        LogFormat::Syslog    => "Syslog",
        LogFormat::Json      => "JSON",
    }.to_owned();
    
    index.format = Some(format_name.clone());

    let records: Vec<LineRecord> = (0..index.len())
        .into_par_iter()
        .map(|i| {
            let line = index.line_str(i);
            match fmt {
                LogFormat::PlainText => plaintext::parse_line(line),
                LogFormat::Logcat    => logcat::parse_line(line),
                LogFormat::Syslog    => syslog::parse_line(line),
                LogFormat::Json      => json::parse_line(line),
            }
        })
        .collect();

    ParsedIndex {
        lines: index,
        records,
        format: format_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLevel;
    use std::path::PathBuf;

    fn temp_file_with_content(content: &str, ext: &str) -> PathBuf {
        use std::io::Write;
        let mut temp_file = tempfile::Builder::new().suffix(ext).tempfile().unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.into_temp_path().keep().unwrap()
    }

    #[test]
    fn parse_index_logcat() {
        let content = "--------- beginning of main\n05-28 14:23:05.123 1234 5678 I Tag: info msg\n05-28 14:23:06.456 1234 5678 E Tag: err msg\n";
        let path = temp_file_with_content(content, ".log");
        let index = crate::loader::index_file(&path).unwrap();
        let parsed = parse_index(index);
        
        assert_eq!(parsed.format, "Logcat");
        assert_eq!(parsed.records.len(), 3);
        
        // Line 0: banner
        assert_eq!(parsed.level_of(0), LogLevel::Unknown);
        assert_eq!(parsed.timestamp_of(0), None);
        
        // Line 1: Info
        assert_eq!(parsed.level_of(1), LogLevel::Info);
        assert_eq!(parsed.timestamp_of(1), Some("05-28 14:23:05.123"));
        
        // Line 2: Error
        assert_eq!(parsed.level_of(2), LogLevel::Error);
        assert_eq!(parsed.timestamp_of(2), Some("05-28 14:23:06.456"));
    }

    #[test]
    fn parse_index_syslog() {
        let content = "Jan 15 14:23:05 host p: msg";
        let path = temp_file_with_content(content, ".txt");
        let index = crate::loader::index_file(&path).unwrap();
        let parsed = parse_index(index);
        
        assert_eq!(parsed.format, "Syslog");
    }
}
