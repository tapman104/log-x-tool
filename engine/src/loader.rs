use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::types::{AppError, LineIndex, LogEntry, LogFile, LogLevel};

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
///
/// # Deprecation
/// For large files (> a few hundred MB) prefer [`index_file`], which avoids
/// cloning every line into a `String` and operates in O(lines × 8 bytes) memory.
#[deprecated(note = "use index_file for large files")]
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

// ---------------------------------------------------------------------------
// index_file — zero-copy large-file loader
// ---------------------------------------------------------------------------

/// The size of each chunk scanned when building the line-offset table.
const CHUNK: usize = 64 * 1024; // 64 KiB

/// Memory-map `path` and build a [`LineIndex`] of byte offsets.
///
/// This function never copies line content; it stores only one `u64` per line.
/// A 100 GB file with 1 billion lines would consume ~8 GB for the offset table,
/// but the *file* data itself is paged in on demand by the OS.
///
/// # Errors
/// Returns [`AppError::Io`] if the file cannot be opened or memory-mapped.
pub fn index_file<P: AsRef<Path>>(path: P) -> Result<LineIndex, AppError> {
    index_file_with_progress(path, |_, _| {})
}

pub fn index_file_with_progress<P, F>(
    path: P,
    progress: F,
) -> Result<LineIndex, AppError>
where
    P: AsRef<Path>,
    F: Fn(u64, u64) + Send + 'static,
{
    let path = path.as_ref();
    let file = File::open(path)?;

    // Safety: we only ever read from the map; the file is not mutated.
    // On Windows and Unix, a read-only map of a file opened for reading is safe
    // as long as no other process truncates the file — the standard caveat for mmap.
    let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };

    let mut offsets: Vec<u64> = Vec::new();

    if !mmap.is_empty() {
        // Line 0 always starts at byte 0.
        offsets.push(0);

        // Scan in 64 KiB chunks for '\n' characters.
        let len = mmap.len();
        let mut pos = 0usize;
        while pos < len {
            progress(pos as u64, len as u64);
            let end = (pos + CHUNK).min(len);
            let chunk = &mmap[pos..end];
            for (local_i, &b) in chunk.iter().enumerate() {
                if b == b'\n' {
                    let next_line_start = (pos + local_i + 1) as u64;
                    // Only push if there are bytes after this newline
                    // (avoids a phantom empty line at EOF).
                    if next_line_start < len as u64 {
                        offsets.push(next_line_start);
                    }
                }
            }
            pos = end;
        }
        progress(len as u64, len as u64);
    }

    Ok(LineIndex {
        path: path.to_path_buf(),
        mmap,
        offsets,
        format: None,
    })
}

// ---------------------------------------------------------------------------
// append_new_lines — incremental follow-mode updater
// ---------------------------------------------------------------------------

/// Extend `parsed` with any lines appended to its source file since the last
/// index was built.
///
/// # What this does
/// 1. Re-opens the source file and creates a fresh memory map (O(1) — no I/O).
/// 2. Scans the bytes beyond the previously indexed region for `\n` characters.
/// 3. Appends new byte offsets to `parsed.lines.offsets`.
/// 4. Parses each new line into a [`crate::types::LineRecord`] and appends it
///    to `parsed.records`, using the format already detected for this file.
///
/// # Return value
/// Returns the 0-based index of the first newly added line inside
/// `parsed.lines.offsets`, so the caller can update its filter bitmap for
/// exactly those lines.  Returns `None` when:
/// - The file has not grown (or is unavailable).
/// - New bytes exist but contain no complete line yet (i.e. no `\n` found).
///
/// # Caller contract
/// The caller must ensure exclusive (`&mut`) access to `parsed`; typically via
/// `Arc::get_mut`.  The function never panics on a valid `ParsedIndex`.
pub fn append_new_lines(parsed: &mut crate::types::ParsedIndex) -> Option<usize> {
    let path = parsed.lines.path.clone();

    // Open the file and build a fresh mmap — this is O(1) (OS page-table only).
    let file = std::fs::File::open(&path).ok()?;
    // Safety: read-only map; the file is not mutated by this process.
    let new_mmap = unsafe { memmap2::MmapOptions::new().map(&file).ok()? };

    let old_len = parsed.lines.mmap.len();
    let new_len = new_mmap.len();

    if new_len <= old_len {
        // No growth — caller should treat this as "no rotation" since size
        // comparisons for rotation happen before calling this function.
        return None;
    }

    // Record where new lines will start in the offsets vector.
    let first_new_line_idx = parsed.lines.offsets.len();

    // Scan only the newly appended bytes.
    let new_bytes = &new_mmap[old_len..new_len];

    // If old content ended on a newline (or file was previously empty), the
    // byte at `old_len` is the first byte of a brand-new line.
    // If it did NOT end on a newline, `old_len` is a continuation of the last
    // already-indexed line; we only push offsets for lines that start AFTER
    // the next `\n` we find.
    let old_ended_with_newline = old_len == 0
        || new_mmap.get(old_len.wrapping_sub(1)) == Some(&b'\n');

    let mut new_offsets: Vec<u64> = Vec::new();
    if old_ended_with_newline && !new_bytes.is_empty() {
        new_offsets.push(old_len as u64);
    }
    for (local_i, &b) in new_bytes.iter().enumerate() {
        if b == b'\n' {
            let next_start = old_len + local_i + 1;
            // Only push if there are bytes after this newline.
            if next_start < new_len {
                new_offsets.push(next_start as u64);
            }
        }
    }

    if new_offsets.is_empty() {
        // New bytes arrived but no complete line yet.  Still replace the mmap
        // so the last (partial) line shows the latest bytes in the UI.
        parsed.lines.mmap = new_mmap;
        return None;
    }

    // Replace the mmap first so that `line_bytes` / `line_str` can see the
    // new content as we parse it below.
    parsed.lines.mmap = new_mmap;
    parsed.lines.offsets.extend_from_slice(&new_offsets);

    // Parse new records with the format already established for this file.
    let format = parsed.format.clone();
    let end = parsed.lines.offsets.len();
    for i in first_new_line_idx..end {
        let line = parsed.lines.line_str(i);
        let record = crate::parser::parse_record(line, &format);
        parsed.records.push(record);
    }

    Some(first_new_line_idx)
}

// ---------------------------------------------------------------------------
// Tests for index_file / LineIndex
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Helper: write content to a temp file and return the path.
    fn write_temp(content: &[u8]) -> (tempfile::NamedTempFile, std::path::PathBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        let path = f.path().to_path_buf();
        (f, path) // keep `f` alive so the file isn't deleted
    }

    #[test]
    fn index_line_count_lf() {
        let content = b"alpha\nbeta\ngamma\n";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        assert_eq!(idx.len(), 3, "expected 3 lines for 3-line LF file");
    }

    #[test]
    fn index_first_line_content() {
        let content = b"first line\nsecond line\nthird line\n";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        assert_eq!(idx.line_str(0), "first line");
    }

    #[test]
    fn index_last_line_no_trailing_newline() {
        // Last line has no terminating newline — common for non-Unix files.
        let content = b"line one\nline two\nline three";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        let last = idx.len() - 1;
        assert_eq!(idx.line_str(last), "line three");
    }

    #[test]
    fn index_crlf_stripped() {
        let content = b"foo\r\nbar\r\nbaz\r\n";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.line_str(0), "foo");
        assert_eq!(idx.line_str(1), "bar");
        assert_eq!(idx.line_str(2), "baz");
    }

    #[test]
    fn index_empty_file() {
        let content = b"";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        assert!(idx.is_empty(), "empty file must yield zero offsets");
    }

    #[test]
    fn index_single_line_no_newline() {
        let content = b"only line";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.line_str(0), "only line");
    }

    #[test]
    fn index_all_lines_match_split() {
        // Cross-check every line against Rust's built-in lines() iterator.
        let content = b"apple\nbanana\ncherry\ndate\nelder\n";
        let (_f, path) = write_temp(content);
        let idx = index_file(&path).unwrap();
        let expected: Vec<&str> = std::str::from_utf8(content)
            .unwrap()
            .lines()
            .collect();
        assert_eq!(idx.len(), expected.len());
        for (i, exp) in expected.iter().enumerate() {
            assert_eq!(idx.line_str(i), *exp, "mismatch at line {i}");
        }
    }
}
