Prompt Plan: Log Viewer in Rust + egui
Here's the exact build order, file by file. Each prompt = one session with me.

Prompt 1 — Workspace Skeleton
"Create a Cargo workspace with two crates:
 `engine` (lib) and `ui` (bin).
 No logic yet. Just Cargo.toml files,
 mod structure, and a window that opens
 and says 'Log Viewer' using egui/eframe."
Files born: Cargo.toml, engine/src/lib.rs, ui/src/main.rs

Prompt 2 — Core Types
"In engine/src/types.rs define:

- LogEntry { line_number, raw, timestamp Option }
- LogFile { path, entries Vec<LogEntry> }
- AppError enum
 Nothing else. No parsing yet."
Files born: engine/src/types.rs

Prompt 3 — File Loading
"In engine/src/loader.rs write a function
 load_file(path) -> Result<LogFile, AppError>
 that reads a text file line by line into
 LogEntry vec. No format detection yet,
 treat every line as raw."
Files born: engine/src/loader.rs

Prompt 4 — UI File Drop + Open
"In ui/src/app.rs build an egui App struct
 that holds Option<LogFile>.
 Support drag-and-drop file onto window
 and a File > Open button.
 Call engine::loader::load_file on drop/open.
 Show entry count when loaded."
Files born: ui/src/app.rs

Prompt 5 — Log Table View
"In ui/src/log_panel.rs render the LogFile
 entries in an egui TableBody.
 Columns: line number, raw text.
 Virtual scrolling via egui's show_rows.
 Handle 100k lines without lag."
Files born: ui/src/log_panel.rs

Prompt 6 — Polish Pass
"Add to ui/src/app.rs:

- dark/light theme toggle
- file name in title bar
- empty state message when no file loaded
- loading indicator for large files (spawn thread)"
No new files. Edits only.

Final File Map After All 6 Prompts
logvault/
├── Cargo.toml              (workspace)
├── engine/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          (re-exports)
│       ├── types.rs        (LogEntry, LogFile, AppError)
│       └── loader.rs      (load_file)
└── ui/
    ├── Cargo.toml
    └── src/
        ├── main.rs         (eframe entry)
        ├── app.rs          (App struct, drag-drop, open)
        └── log_panel.rs   (table renderer)
