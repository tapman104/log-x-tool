# 🚀 Log Analyzer

![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg?style=flat-square&logo=rust)
![License](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg?style=flat-square)

A blazingly fast, native log viewing and analysis tool written in **Rust**. Log Analyzer is designed to effortlessly handle gigabytes of log files with memory-efficient indexing, parallel parsing, and a highly responsive graphical user interface.

## 🏗️ Project Structure

The workspace is divided into two primary crates:

- **`engine`**: The core library crate responsible for memory-mapped file I/O, format detection, parallel parsing, and caching.
- **`ui`**: The graphical frontend built with `eframe` and `egui`, providing an immediate-mode GUI for searching, filtering, and live-tailing logs.

---

## ✨ Current Features

### ⚙️ Core & Engine

- **Memory-Mapped I/O**: Opens massive log files (GBs in size) efficiently without loading the entire contents into RAM.
- **Parallel Parsing**: Leverages `rayon` to parse log records across multiple CPU cores, achieving extremely high throughput.
- **Auto-Format Detection**: Intelligently sniffs file extensions and initial contents to determine log formats. Currently supports:
  - **Android Logcat** (`tag PID TID level msg`)
  - **Syslog** (BSD/Linux format)
  - **JSON / NDJSON** (Newline-Delimited JSON)
  - **Plain Text** (Fallback)
- **Aggressive Caching**: Persists parsed index metadata to disk. Opening a previously indexed file is practically instantaneous.

### 🖥️ User Interface (UI)

- **Responsive GUI**: Built on `egui` to ensure 60fps rendering regardless of the log file size.
- **High-Performance Filtering**: Utilizes `RoaringBitmap` to process searches and filter by log levels instantly across millions of rows.
- **Live Follow Mode**: Watches the log file for appended changes and automatically tails the output, handling file rotations seamlessly.
- **Search & Jump**: Full-text searching across all logs and quick jumping to specific line numbers.
- **Dark/Light Themes**: Native support for switching between dark and light appearance.
- **Exporting**: Save filtered, specific subsets of logs to a new file instantly.
- **Drag & Drop**: Simply drag a log file into the window to open it.

---

## 🔮 Next Features

Here is a list of planned enhancements and upcoming features for Log Analyzer:

### 🔍 Advanced Search & Filtering

- **Regular Expressions**: Add support for full Regex in the search bar.
- **Multi-term Search**: Ability to filter by multiple inclusive/exclusive terms (e.g., `ERROR AND (database OR network)`).
- **Time-Range Filtering**: Filter logs strictly by timestamp ranges, utilizing the parsed date-times.

### 🎨 Customization & Parsing

- **Custom Grok/Regex Patterns**: Allow users to define their own log parsing rules in a config file for proprietary log formats.
- **Syntax Highlighting**: Add syntax highlighting for JSON payloads, stack traces, and recognized identifiers inside log messages.
- **Column Customization**: Let users toggle visibility, resize, and reorder table columns.

### 📊 Analysis & Visualizations

- **Log Timeline Bar Chart**: A visual histogram at the top or bottom of the screen showing error/warning frequency over time to quickly spot incident spikes.
- **Log Aggregation/Stats**: Group similar logs to find the most frequent errors or messages.

### ☁️ Remote & Cloud Integration

- **SSH Tailing**: Connect directly to remote servers via SSH to stream and analyze logs locally.
- **Cloud Bucket Support**: Stream and index logs directly from AWS S3, Google Cloud Storage, or Azure Blob Storage.

---

## 🛠️ Building and Running

Ensure you have [Rust](https://www.rust-lang.org/tools/install) installed.

To run the user interface:

```bash
cargo run --release --bin log-viewer
```

To build a release executable:

```bash
cargo build --release
```

The compiled binary will be available in `target/release/`.
