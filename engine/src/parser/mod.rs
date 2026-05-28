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
