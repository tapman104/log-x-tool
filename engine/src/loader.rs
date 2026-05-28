use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::types::{AppError, LogEntry, LogFile, LogLevel};

/// Detect the severity level of a log line by scanning for level keywords.
///
/// Checks are ordered highest-to-lowest so that the dominant keyword wins
/// on lines that happen to contain multiple level words (e.g. "ERROR in info handler").
/// Returns `Some(LogLevel::Unknown)` when no keyword is recognised so that the
/// `level` field is always populated — callers can tell the difference between
/// "not yet checked" (`None`) and "checked, no match" (`Unknown`).
///
/// Matching is case-insensitive and uses plain substring search — no regex, no deps.
fn detect_level(line: &str) -> Option<LogLevel> {
    // A single lowercase copy avoids repeated allocations for each keyword test.
    let lower = line.to_lowercase();

    if lower.contains("error") {
        Some(LogLevel::Error)
    } else if lower.contains("warn") {
        Some(LogLevel::Warn)
    } else if lower.contains("info") {
        Some(LogLevel::Info)
    } else if lower.contains("debug") {
        Some(LogLevel::Debug)
    } else if lower.contains("trace") {
        Some(LogLevel::Trace)
    } else {
        Some(LogLevel::Unknown)
    }
}

/// Read every line of `path` into a [`LogFile`].
///
/// - Lines are 1-indexed.
/// - Every line is stored verbatim in `raw`; no format detection.
/// - `timestamp` is left as `None`; a later pass can fill it in.
/// - `level` is populated by [`detect_level`] on every line.
/// - Returns [`AppError::Io`] on any I/O failure.
pub fn load_file<P: AsRef<Path>>(path: P) -> Result<LogFile, AppError> {
    let path = path.as_ref();

    let file = File::open(path)?; // AppError::Io via From<io::Error>
    let reader = BufReader::new(file);

    let mut entries = Vec::new();

    for (index, line_result) in reader.lines().enumerate() {
        let raw = line_result?; // propagate any mid-read I/O error
        let level = detect_level(&raw);
        entries.push(LogEntry {
            line_number: index + 1, // 1-based
            level,
            raw,
            timestamp: None,
        });
    }

    Ok(LogFile {
        path: path.to_path_buf(),
        entries,
        format: None,
    })
}
