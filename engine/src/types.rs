use std::path::PathBuf;

/// Severity level detected from a log line.
#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    /// Line was inspected but no recognisable level keyword was found.
    Unknown,
}

/// A single line read from a log file.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// 1-based line number within the source file.
    pub line_number: usize,
    /// The raw, unmodified text of the line.
    pub raw: String,
    /// Optional parsed timestamp (populated by the parser in a later prompt).
    pub timestamp: Option<String>,
    /// Severity level detected by the loader, if any.
    pub level: Option<LogLevel>,
}

/// The in-memory representation of a loaded log file.
#[derive(Debug, Clone)]
pub struct LogFile {
    /// Absolute path to the source file on disk.
    pub path: PathBuf,
    /// All entries in order, one per line.
    pub entries: Vec<LogEntry>,
    /// Human-readable name of the detected log format, e.g. `"Plain text"`,
    /// `"Syslog"`, `"Logcat"`, `"JSON"`.  Set by the parser; `None` if
    /// `parse_file` has not been called yet.
    pub format: Option<String>,
}

/// All errors the engine can surface to the UI.
#[derive(Debug)]
pub enum AppError {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// The supplied path is not valid UTF-8.
    InvalidPath(String),
    /// Any other unexpected failure (carries a human-readable message).
    Other(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Io(e) => write!(f, "I/O error: {e}"),
            AppError::InvalidPath(p) => write!(f, "Invalid path: {p}"),
            AppError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e)
    }
}
