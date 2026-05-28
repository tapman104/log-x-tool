// engine — core library crate.
pub mod types;
pub mod loader;
pub mod parser;

// Re-export top-level so callers can write `engine::LogEntry` / `engine::load_file` etc.
pub use types::{AppError, LogEntry, LogFile, LogLevel};
pub use loader::load_file;
pub use parser::parse_file;
