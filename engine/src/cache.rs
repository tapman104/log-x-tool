//! Sidecar index cache for `ParsedIndex`.
//!
//! # File layout
//!
//! ```text
//! ┌──────────────────────────────── Header (32 bytes) ──────────────────────┐
//! │  [0..8]   magic      : b"LOGVIDX1"                                      │
//! │  [8..16]  file_size  : u64 le  — byte length of the source file         │
//! │  [16..24] mtime_secs : u64 le  — last-modified Unix timestamp (seconds) │
//! │  [24..32] line_count : u64 le                                            │
//! ├──────────────────────────────── Body ───────────────────────────────────┤
//! │  offsets  : line_count × u64 le  (byte offsets into the source file)    │
//! │  records  : line_count × 4 bytes (level:u8 ts_start:u16le ts_len:u8)   │
//! ├──────────────────────────────── Footer ─────────────────────────────────┤
//! │  format_len : u8                                                         │
//! │  format_str : format_len bytes (UTF-8, no NUL)                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The cache is keyed by a hash of the **absolute** source path and validated
//! by comparing `file_size` and `mtime_secs` against live `fs::metadata`.
//! Any mismatch causes `try_load` to return `None`; the caller falls back to
//! a full parse and then calls `save` to refresh the cache.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::types::{LineIndex, LineRecord, LogLevel, ParsedIndex};

// ── Magic & constants ───────────────────────────────────────────────────────

const MAGIC: &[u8; 8] = b"LOGVIDX1";
const HEADER_LEN: usize = 32;
/// Packed size of one `LineRecord` on disk (level:u8 + ts_start:u16 + ts_len:u8).
const RECORD_BYTES: usize = 4;

// ── LogLevel ↔ u8 ──────────────────────────────────────────────────────────

fn level_to_u8(l: LogLevel) -> u8 {
    match l {
        LogLevel::Error   => 0,
        LogLevel::Warn    => 1,
        LogLevel::Info    => 2,
        LogLevel::Debug   => 3,
        LogLevel::Trace   => 4,
        LogLevel::Unknown => 5,
    }
}

fn level_from_u8(b: u8) -> LogLevel {
    match b {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        3 => LogLevel::Debug,
        4 => LogLevel::Trace,
        _ => LogLevel::Unknown,
    }
}

// ── Step 1 — Cache file path ────────────────────────────────────────────────

/// Derive a stable cache path for `source` inside the OS cache directory.
///
/// The filename is a hex-encoded hash of the *absolute* path so that two
/// files with the same name in different directories get different cache
/// entries.  Returns `None` when `dirs::cache_dir()` is unavailable.
pub fn cache_path(source: &Path) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    // Canonicalise before hashing so that `./foo` and `/abs/foo` match.
    let canonical = source.canonicalize().unwrap_or_else(|_| source.to_path_buf());
    Hash::hash(&canonical, &mut hasher);
    let hash = hasher.finish();

    dirs::cache_dir().map(|mut p| {
        p.push("log-viewer");
        p.push(format!("{:016x}.idx", hash));
        p
    })
}

// ── Helpers — little-endian I/O ─────────────────────────────────────────────

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn write_u64_le(w: &mut impl Write, v: u64) -> std::io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

// ── Step 3 — try_load ───────────────────────────────────────────────────────

/// Attempt to load a cached `ParsedIndex` for `source`.
///
/// Returns `None` if:
/// - No cache file exists for this path.
/// - The cache magic or format is wrong.
/// - The cached `file_size` or `mtime_secs` does not match current metadata
///   (i.e. the source file was modified since the cache was written).
/// - Any I/O error occurs while reading the cache.
///
/// Memory usage: O(line_count × 12 bytes) for the offsets + records Vecs.
/// The old `std::fs::read` implementation allocated a second copy of the
/// entire cache (~1.2 GB for 100 M lines) on top of that.  This version
/// streams directly into the destination Vecs without any extra buffer.
pub fn try_load(source: &Path) -> Option<ParsedIndex> {
    use std::io::{BufReader, Read};

    let cp = cache_path(source)?;
    let cache_file = std::fs::File::open(&cp).ok()?;
    let mut r = BufReader::new(cache_file);

    // ── Read and validate the 32-byte header ────────────────────────────────
    let mut hdr = [0u8; HEADER_LEN];
    r.read_exact(&mut hdr).ok()?;

    if &hdr[0..8] != MAGIC {
        return None;
    }
    let cached_file_size  = read_u64_le(&hdr,  8);
    let cached_mtime_secs = read_u64_le(&hdr, 16);
    let line_count        = read_u64_le(&hdr, 24) as usize;

    // ── Validate freshness against live metadata ─────────────────────────────
    let meta = std::fs::metadata(source).ok()?;
    let live_file_size = meta.len();
    let live_mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if live_file_size != cached_file_size || live_mtime_secs != cached_mtime_secs {
        // Cache is stale — discard after only 32 bytes read.
        return None;
    }

    // ── Stream offsets directly into a pre-allocated Vec ────────────────────
    let mut offsets = Vec::with_capacity(line_count);
    {
        let mut buf = [0u8; 8];
        for _ in 0..line_count {
            r.read_exact(&mut buf).ok()?;
            offsets.push(u64::from_le_bytes(buf));
        }
    }

    // ── Stream records directly into a pre-allocated Vec ────────────────────
    let mut records = Vec::with_capacity(line_count);
    {
        let mut buf = [0u8; RECORD_BYTES];
        for _ in 0..line_count {
            r.read_exact(&mut buf).ok()?;
            records.push(LineRecord {
                level:    level_from_u8(buf[0]),
                ts_start: u16::from_le_bytes([buf[1], buf[2]]),
                ts_len:   buf[3],
            });
        }
    }

    // ── Read the footer (format string) ─────────────────────────────────────
    let mut fmt_len_buf = [0u8; 1];
    r.read_exact(&mut fmt_len_buf).ok()?;
    let fmt_len = fmt_len_buf[0] as usize;
    let mut fmt_buf = vec![0u8; fmt_len];
    r.read_exact(&mut fmt_buf).ok()?;
    let format = std::str::from_utf8(&fmt_buf).ok()?.to_owned();

    // ── Memory-map the source file ───────────────────────────────────────────
    let file = std::fs::File::open(source).ok()?;
    // Safety: read-only map; we do not mutate the file.
    let mmap = unsafe { memmap2::MmapOptions::new().map(&file).ok()? };

    let lines = LineIndex {
        path: source.to_path_buf(),
        mmap,
        offsets,
        format: Some(format.clone()),
    };

    Some(ParsedIndex { lines, records, format })
}

// ── Step 4 — save ───────────────────────────────────────────────────────────

/// Persist `parsed` as a sidecar cache file for `source`.
///
/// Writes to a `.tmp` sibling first, then renames atomically so that a crash
/// or power loss never leaves a half-written cache file in place.
///
/// Errors are silently ignored — a missing cache just means a slower next open.
pub fn save(parsed: &ParsedIndex, source: &Path) {
    if let Err(e) = save_inner(parsed, source) {
        // Non-fatal: log to stderr in debug builds only.
        #[cfg(debug_assertions)]
        eprintln!("[cache] save failed for {}: {e}", source.display());
        let _ = e; // suppress unused-variable warning in release
    }
}

fn save_inner(parsed: &ParsedIndex, source: &Path) -> std::io::Result<()> {
    let cp = cache_path(source).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "cannot determine cache dir")
    })?;

    // Ensure the cache directory exists.
    if let Some(parent) = cp.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Gather metadata for the header.
    let meta = std::fs::metadata(source)?;
    let file_size = meta.len();
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let line_count = parsed.lines.len() as u64;

    // ── Write to a .tmp file for atomic replace ──────────────────────────────
    let tmp_path = cp.with_extension("tmp");
    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut w = std::io::BufWriter::new(file);

        // Header
        w.write_all(MAGIC)?;
        write_u64_le(&mut w, file_size)?;
        write_u64_le(&mut w, mtime_secs)?;
        write_u64_le(&mut w, line_count)?;

        // Offsets
        for &off in &parsed.lines.offsets {
            write_u64_le(&mut w, off)?;
        }

        // Records (4 bytes each: level u8, ts_start u16 le, ts_len u8)
        for rec in &parsed.records {
            w.write_all(&[
                level_to_u8(rec.level),
                (rec.ts_start & 0xFF) as u8,
                (rec.ts_start >> 8)   as u8,
                rec.ts_len,
            ])?;
        }

        // Footer
        let fmt_bytes = parsed.format.as_bytes();
        let fmt_len = fmt_bytes.len().min(255) as u8;
        w.write_all(&[fmt_len])?;
        w.write_all(&fmt_bytes[..fmt_len as usize])?;

        w.flush()?;
    }

    // Atomic rename: on the same filesystem this is guaranteed to be atomic
    // on POSIX; on Windows it is best-effort (no cross-device issue here).
    std::fs::rename(&tmp_path, &cp)?;

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLevel;

    /// Build a minimal `ParsedIndex` backed by a real temp file so the mmap
    /// and metadata checks inside `try_load` work correctly.
    fn make_parsed(content: &[u8], format: &str) -> (tempfile::NamedTempFile, ParsedIndex) {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();

        let path = f.path().to_path_buf();
        let li = crate::loader::index_file(&path).unwrap();
        let parsed = crate::parser::parse_index(li);

        // We return the NamedTempFile to keep the file alive.
        let parsed = ParsedIndex {
            format: format.to_owned(),
            ..parsed
        };
        (f, parsed)
    }

    #[test]
    fn roundtrip_save_and_load() {
        let content = b"2024-01-01 ERROR something bad\n2024-01-01 INFO all good\n";
        let (tmp, parsed) = make_parsed(content, "Plain text");
        let source = tmp.path();

        // Save
        save(&parsed, source);

        // Load — must hit cache
        let loaded = try_load(source).expect("cache should be present after save");

        assert_eq!(loaded.format, parsed.format);
        assert_eq!(loaded.lines.len(), parsed.lines.len());
        assert_eq!(loaded.lines.offsets, parsed.lines.offsets);

        for i in 0..parsed.records.len() {
            assert_eq!(
                loaded.records[i].level, parsed.records[i].level,
                "level mismatch at record {i}"
            );
            assert_eq!(
                loaded.records[i].ts_start, parsed.records[i].ts_start,
                "ts_start mismatch at record {i}"
            );
            assert_eq!(
                loaded.records[i].ts_len, parsed.records[i].ts_len,
                "ts_len mismatch at record {i}"
            );
        }
    }

    #[test]
    fn stale_on_file_change() {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"INFO hello\n").unwrap();
        f.flush().unwrap();

        let (_, parsed) = make_parsed(b"INFO hello\n", "Plain text");
        // Copy the path into an owned value before mutably borrowing `f`.
        let source_path = f.path().to_path_buf();
        save(&parsed, &source_path);

        // Modify the file — size changes → cache must be stale.
        f.write_all(b"ERROR new line\n").unwrap();
        f.flush().unwrap();

        assert!(
            try_load(&source_path).is_none(),
            "cache must be None after source file modification"
        );
    }

    #[test]
    fn level_roundtrip_all_variants() {
        use LogLevel::*;
        for lvl in [Error, Warn, Info, Debug, Trace, Unknown] {
            assert_eq!(level_from_u8(level_to_u8(lvl)), lvl);
        }
    }

    #[test]
    fn no_cache_returns_none() {
        // A path that was never saved — try_load must return None gracefully.
        let result = try_load(Path::new("/nonexistent/definitely/not/a/real/file.log"));
        assert!(result.is_none());
    }
}
