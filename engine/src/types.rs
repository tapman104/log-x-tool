use std::path::PathBuf;

/// Severity level detected from a log line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    /// Line was inspected but no recognisable level keyword was found.
    Unknown,
}

impl From<LogLevel> for usize {
    fn from(lvl: LogLevel) -> Self {
        match lvl {
            LogLevel::Error => 0,
            LogLevel::Warn => 1,
            LogLevel::Info => 2,
            LogLevel::Debug => 3,
            LogLevel::Trace => 4,
            LogLevel::Unknown => 5,
        }
    }
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

// ---------------------------------------------------------------------------
// LineIndex — zero-copy large-file support
// ---------------------------------------------------------------------------

/// A memory-mapped, offset-indexed view of a log file.
///
/// Instead of cloning every line into a `String`, `LineIndex` stores only the
/// byte offset of each line start and lets the OS page cache do the heavy
/// lifting.  Suitable for files of any size (including 100+ GB).
pub struct LineIndex {
    /// Absolute path to the source file.
    pub path: std::path::PathBuf,
    /// Read-only memory map of the entire file.
    pub mmap: memmap2::Mmap,
    /// Byte offset of the first byte of each line (0-indexed line numbers).
    /// `offsets[i]` is the start of line `i`; the end is `offsets[i+1]` or
    /// `mmap.len()` for the last line.
    pub offsets: Vec<u64>,
    /// Human-readable detected format name (set by the parser layer).
    pub format: Option<String>,
}

impl LineIndex {
    /// Return the raw bytes of line `i`, with any trailing `\r\n` stripped.
    ///
    /// This is a zero-copy borrow into the memory map — no allocation.
    pub fn line_bytes(&self, i: usize) -> &[u8] {
        let start = self.offsets[i] as usize;
        let end = if i + 1 < self.offsets.len() {
            self.offsets[i + 1] as usize
        } else {
            self.mmap.len()
        };
        let raw = &self.mmap[start..end];
        // Strip trailing newline characters (handles CRLF, LF, and bare CR).
        raw.strip_suffix(b"\r\n")  // CRLF — must be tried first
            .or_else(|| raw.strip_suffix(b"\n"))  // plain LF
            .or_else(|| raw.strip_suffix(b"\r"))  // bare CR (old Mac)
            .unwrap_or(raw)
    }

    /// Return line `i` as a `&str`.
    ///
    /// Returns `""` for any line whose bytes are not valid UTF-8; the caller
    /// is responsible for any lossy-conversion handling it requires.
    pub fn line_str(&self, i: usize) -> &str {
        std::str::from_utf8(self.line_bytes(i)).unwrap_or("")
    }

    /// Total number of indexed lines.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// `true` when the file contained no lines (empty file).
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Streaming parser types
// ---------------------------------------------------------------------------

/// Compact parsed metadata for one line.  Stored in a flat Vec for O(1) access.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineRecord {
    pub level: LogLevel,
    /// Byte offset within the line where the timestamp starts, and its length.
    /// Both are 0 when no timestamp was found.
    pub ts_start: u16,
    pub ts_len:   u8,
}

pub struct ParsedIndex {
    pub lines:   LineIndex,
    pub records: Vec<LineRecord>,  // parallel to lines.offsets
    pub format:  String,
}

impl ParsedIndex {
    pub fn timestamp_of(&self, i: usize) -> Option<&str> {
        let r = self.records[i];
        if r.ts_len == 0 { return None; }
        let line = self.lines.line_str(i);
        let start = r.ts_start as usize;
        let end   = start + r.ts_len as usize;
        line.get(start..end)
    }
    
    pub fn level_of(&self, i: usize) -> LogLevel {
        self.records[i].level
    }
}
