// engine — core library crate.
pub mod types;
pub mod loader;
pub mod parser;
pub mod cache;

// Re-export top-level so callers can write `engine::LogEntry` / `engine::load_file` etc.
pub use types::{AppError, LineIndex, ParsedIndex, LogEntry, LogFile, LogLevel};
#[allow(deprecated)]
pub use loader::{index_file, index_file_with_progress, load_file, append_new_lines};
pub use parser::{parse_file, parse_index, parse_record};
