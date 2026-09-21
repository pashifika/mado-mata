use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::field::{Field, Visit};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

const GUI_QUEUE_CAPACITY: usize = 256;
const FILE_QUEUE_CAPACITY: usize = 256;
const FILE_BYTES: u64 = 1024 * 1024;
const FILE_NAMES: [&str; 3] = [
    "application.jsonl",
    "application.1.jsonl",
    "application.2.jsonl",
];
const MESSAGE_BYTES: usize = 1024;
const VALUE_BYTES: usize = 4096;
const VALUE_NODES: usize = 64;
const FIELD_BYTES: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);
const REDACTED: &str = "[redacted]";
const TRUNCATED: &str = "[truncated]";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub sequence: u64,
    pub time_ms: u64,
    pub source: String,
    pub level: String,
    pub run: Option<String>,
    pub code: String,
    pub message: String,
    pub fields: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogBatch {
    pub entries: Vec<LogEntry>,
    pub gui_dropped: u64,
    pub file_dropped: u64,
    pub file_errors: u64,
    pub last_file_error: Option<String>,
}

/// Counters are cumulative; draining GUI entries never clears sink status.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogStatus {
    pub gui_dropped: u64,
    pub file_dropped: u64,
    pub file_errors: u64,
    pub last_file_error: Option<String>,
    pub file_written: u64,
    pub file_pending: usize,
    pub accepting: bool,
    pub shutdown_complete: bool,
    pub shutdown_timed_out: bool,
}

pub struct Logger {
    layer: OutputLayer,
    dispatch: Dispatch,
    // Retain the flush guard for the backend lifetime, not an event callback.
    worker: Mutex<Option<WorkerGuard>>,
}

impl Logger {
    /// File initialization runs off-thread; its failure leaves GUI logging usable.
    pub fn new(directory: PathBuf) -> Result<Self, Fault> {
        let output = Arc::new(Output::new());
        let worker_output = Arc::clone(&output);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("application-log-writer".into())
            .spawn(move || {
                writer_loop(&worker_output, RotatingFile::new(directory));
                let _ = done_tx.send(());
            })
            .map_err(|_| {
                Fault::new(
                    "LoggingInitialization",
                    "Cannot start application log writer",
                )
            })?;
        let layer = OutputLayer { output };
        let dispatch = Dispatch::new(Registry::default().with(layer.clone()));
        Ok(Self {
            layer,
            dispatch,
            worker: Mutex::new(Some(WorkerGuard {
                thread: Some(thread),
                done: done_rx,
            })),
        })
    }

    /// Import owned Script values without flattening or parsing display text.
    pub fn emit(
        &self,
        source: &str,
        level: &str,
        run: Option<&str>,
        code: &str,
        message: &str,
        fields: Value,
    ) {
        self.layer.emit(source, level, run, code, message, fields);
    }

    /// Install this dispatch explicitly on every thread emitting Rust diagnostics.
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }

    pub fn with_dispatch<T>(&self, action: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, action)
    }

    pub fn drain(&self) -> LogBatch {
        self.layer.output.drain()
    }

    pub fn status(&self) -> LogStatus {
        self.layer.output.status()
    }

    /// At most 500 ms of writer waiting; timed-out OS I/O is not claimed flushed.
    pub fn shutdown(&self) -> LogStatus {
        self.layer.output.close();
        let worker = lock(&self.worker).take();
        if let Some(mut worker) = worker {
            if !worker.finish() {
                lock(&self.layer.output.state).shutdown_timed_out = true;
            }
        }
        self.status()
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct WorkerGuard {
    thread: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}

impl WorkerGuard {
    fn finish(&mut self) -> bool {
        if self.thread.is_none() {
            return true;
        }
        let finished = self.done.recv_timeout(SHUTDOWN_TIMEOUT).is_ok();
        // Dropping a handle detaches a stalled OS write; it must not stall Stop.
        self.thread.take();
        finished
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

struct State {
    sequence: u64,
    gui: VecDeque<LogEntry>,
    file: VecDeque<Vec<u8>>,
    gui_dropped: u64,
    file_dropped: u64,
    file_errors: u64,
    last_file_error: Option<String>,
    file_written: u64,
    writing: bool,
    file_available: bool,
    accepting: bool,
    worker_finished: bool,
    shutdown_timed_out: bool,
}

struct Output {
    state: Mutex<State>,
    ready: Condvar,
}

impl Output {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                sequence: 0,
                gui: VecDeque::with_capacity(GUI_QUEUE_CAPACITY),
                file: VecDeque::with_capacity(FILE_QUEUE_CAPACITY),
                gui_dropped: 0,
                file_dropped: 0,
                file_errors: 0,
                last_file_error: None,
                file_written: 0,
                writing: false,
                file_available: true,
                accepting: true,
                worker_finished: false,
                shutdown_timed_out: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn publish(&self, mut entry: LogEntry) {
        let mut state = lock(&self.state);
        if !state.accepting {
            state.gui_dropped = state.gui_dropped.saturating_add(1);
            state.file_dropped = state.file_dropped.saturating_add(1);
            return;
        }
        let Some(sequence) = state.sequence.checked_add(1) else {
            state.gui_dropped = state.gui_dropped.saturating_add(1);
            state.file_dropped = state.file_dropped.saturating_add(1);
            return;
        };
        state.sequence = sequence;
        entry.sequence = sequence;
        entry.time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        // Sequence allocation and both admissions share one ordering lock, never I/O.
        if state.file_available && state.file.len() < FILE_QUEUE_CAPACITY {
            if let Ok(mut line) = serde_json::to_vec(&entry) {
                line.push(b'\n');
                state.file.push_back(line);
                self.ready.notify_one();
            } else {
                state.file_dropped = state.file_dropped.saturating_add(1);
            }
        } else {
            state.file_dropped = state.file_dropped.saturating_add(1);
        }
        if state.gui.len() == GUI_QUEUE_CAPACITY {
            state.gui.pop_front();
            state.gui_dropped = state.gui_dropped.saturating_add(1);
        }
        state.gui.push_back(entry);
    }

    fn drain(&self) -> LogBatch {
        let mut state = lock(&self.state);
        LogBatch {
            entries: state.gui.drain(..).collect(),
            gui_dropped: state.gui_dropped,
            file_dropped: state.file_dropped,
            file_errors: state.file_errors,
            last_file_error: state.last_file_error.clone(),
        }
    }

    fn status(&self) -> LogStatus {
        let state = lock(&self.state);
        LogStatus {
            gui_dropped: state.gui_dropped,
            file_dropped: state.file_dropped,
            file_errors: state.file_errors,
            last_file_error: state.last_file_error.clone(),
            file_written: state.file_written,
            file_pending: state.file.len() + usize::from(state.writing),
            accepting: state.accepting,
            shutdown_complete: !state.accepting && state.worker_finished,
            shutdown_timed_out: state.shutdown_timed_out,
        }
    }

    fn close(&self) {
        lock(&self.state).accepting = false;
        self.ready.notify_all();
    }

    fn fail(&self, operation: &str, error: &io::Error) {
        let mut state = lock(&self.state);
        state.file_errors = state.file_errors.saturating_add(1);
        // Keep OS errors path-free and sanitize any custom recovery guidance.
        let detail = if error.get_ref().is_some() {
            safe_text(&error.to_string(), MESSAGE_BYTES)
        } else {
            format!("{:?}", error.kind())
        };
        state.last_file_error = Some(format!("{operation}: {detail}"));
        state.file_available = false;
        state.writing = false;
        state.file_dropped = state.file_dropped.saturating_add(state.file.len() as u64);
        state.file.clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
struct OutputLayer {
    output: Arc<Output>,
}

impl OutputLayer {
    fn emit(
        &self,
        source: &str,
        level: &str,
        run: Option<&str>,
        code: &str,
        message: &str,
        fields: Value,
    ) {
        let fields = Sanitizer {
            bytes: VALUE_BYTES,
            nodes: VALUE_NODES,
        }
        .value(&fields, 0);
        self.output.publish(LogEntry {
            sequence: 0,
            time_ms: 0,
            source: safe_text(source, FIELD_BYTES),
            level: safe_text(level, FIELD_BYTES),
            run: run.map(|value| safe_text(value, FIELD_BYTES)),
            code: safe_text(code, FIELD_BYTES),
            message: safe_text(message, MESSAGE_BYTES),
            fields,
        });
    }
}

impl<S: Subscriber> Layer<S> for OutputLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut visitor = EventFields { fields: Map::new() };
        event.record(&mut visitor);
        let source = take_text(&mut visitor.fields, "source").unwrap_or_else(|| "Rust".into());
        let code = take_text(&mut visitor.fields, "code")
            .unwrap_or_else(|| event.metadata().target().into());
        let message = take_text(&mut visitor.fields, "message").unwrap_or_default();
        let run = take_text(&mut visitor.fields, "run");
        self.emit(
            &source,
            event.metadata().level().as_str(),
            run.as_deref(),
            &code,
            &message,
            Value::Object(visitor.fields),
        );
    }
}

fn take_text(fields: &mut Map<String, Value>, key: &str) -> Option<String> {
    match fields.remove(key) {
        Some(Value::String(value)) => Some(value),
        _ => None,
    }
}

struct EventFields {
    fields: Map<String, Value>,
}

impl EventFields {
    fn record(&mut self, field: &Field, value: Value) {
        if self.fields.len() < VALUE_NODES && field.name().len() <= FIELD_BYTES {
            let value = if sensitive_name(field.name()) {
                Value::String(REDACTED.into())
            } else {
                value
            };
            self.fields.insert(field.name().into(), value);
        }
    }
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if sensitive_name(field.name()) {
            self.record(field, Value::String(REDACTED.into()));
            return;
        }
        let mut text = LimitedText(String::new());
        let value = if write!(&mut text, "{value:?}").is_ok() {
            text.0
        } else {
            TRUNCATED.into()
        };
        self.record(field, Value::String(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, Value::String(safe_text(value, MESSAGE_BYTES)));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, Value::Bool(value));
    }

    fn record_bytes(&mut self, field: &Field, _value: &[u8]) {
        self.record(field, Value::String(REDACTED.into()));
    }
}

struct LimitedText(String);

impl fmt::Write for LimitedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() > MESSAGE_BYTES.saturating_sub(self.0.len()) {
            return Err(fmt::Error);
        }
        self.0.push_str(value);
        Ok(())
    }
}

struct Sanitizer {
    bytes: usize,
    nodes: usize,
}

impl Sanitizer {
    fn value(&mut self, value: &Value, depth: usize) -> Value {
        if self.nodes == 0 || self.bytes < FIELD_BYTES {
            return Value::String(TRUNCATED.into());
        }
        self.nodes -= 1;
        self.bytes -= 8;
        if depth >= 6 {
            self.bytes = self.bytes.saturating_sub(TRUNCATED.len());
            return Value::String(TRUNCATED.into());
        }
        match value {
            Value::String(value) => {
                let text = safe_text(value, MESSAGE_BYTES.min(self.bytes));
                self.bytes = self.bytes.saturating_sub(text.len());
                Value::String(text)
            }
            Value::Array(values) => {
                let mut result = Vec::new();
                for value in values {
                    if self.nodes == 0 || self.bytes < FIELD_BYTES {
                        result.push(Value::String(TRUNCATED.into()));
                        break;
                    }
                    result.push(self.value(value, depth + 1));
                }
                Value::Array(result)
            }
            Value::Object(values) => {
                let mut result = Map::new();
                for (key, value) in values {
                    if self.nodes == 0 || self.bytes < FIELD_BYTES {
                        break;
                    }
                    self.bytes -= key.len().min(FIELD_BYTES);
                    let safe_key = if key.len() <= FIELD_BYTES
                        && key.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                        }) {
                        key.clone()
                    } else {
                        REDACTED.into()
                    };
                    let safe_value = if sensitive_name(key) || safe_key != *key {
                        self.nodes -= 1;
                        Value::String(REDACTED.into())
                    } else {
                        self.value(value, depth + 1)
                    };
                    result.insert(safe_key, safe_value);
                }
                Value::Object(result)
            }
            value => value.clone(),
        }
    }
}

fn sensitive_name(name: &str) -> bool {
    if name.len() > FIELD_BYTES {
        return true;
    }
    let normalized: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "authorization",
        "cookie",
        "apikey",
        "privatekey",
        "ocr",
        "screenshot",
        "image",
        "pixels",
        "frame",
        "terminal",
        "stdout",
        "stderr",
        "raw",
        "path",
        "directory",
        "homedir",
    ]
    .iter()
    .any(|word| normalized.contains(word))
}

fn safe_text(text: &str, limit: usize) -> String {
    // Do not truncate a sensitive prefix into a seemingly harmless fragment.
    if text.len() > limit {
        return TRUNCATED.into();
    }
    let lower = text.to_ascii_lowercase();
    let credentials_or_payload = [
        "password",
        "passwd",
        "secret",
        "token",
        "credential",
        "authorization",
        "cookie",
        "api_key",
        "api-key",
        "apikey",
        "private key",
        "private_key",
        "bearer ",
        "basic ",
        "-----begin",
        "ghp_",
        "github_pat_",
        "akia",
        "ocr_text",
        "ocr text",
        "ocr result",
        "ocr:",
        "screenshot",
        "data:image",
        "raw_terminal",
        "raw terminal",
        "stdout",
        "stderr",
        "://",
        "eyj",
    ]
    .iter()
    .any(|word| lower.contains(word))
        || lower
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
            .any(|word| word.starts_with("sk-"));
    let bytes = text.as_bytes();
    let absolute_path = text.trim() == "/"
        || bytes.iter().enumerate().any(|(index, byte)| {
            (*byte == b'/'
                && (index == 0
                    || (!bytes[index - 1].is_ascii_alphanumeric()
                        && !matches!(bytes[index - 1], b'_' | b'-' | b'.' | b'/'))))
                || (*byte == b'\\' && (index == 0 || matches!(bytes[index - 1], b':' | b'\\')))
        });
    let raw_record = text.contains('{') || text.contains('}') || text.contains('\0');
    if credentials_or_payload || absolute_path || raw_record {
        REDACTED.into()
    } else {
        text.into()
    }
}

struct RotatingFile {
    directory: PathBuf,
    file: File,
    bytes: u64,
}

impl RotatingFile {
    fn new(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        for name in FILE_NAMES {
            match fs::symlink_metadata(directory.join(name)) {
                Ok(metadata) if !metadata.is_file() || metadata.len() > FILE_BYTES => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid log file",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let mut file = Self::open(&directory, false)?;
        let bytes = file.metadata()?.len();
        if bytes > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Incomplete log record; preserve or move the existing file before retrying",
                ));
            }
        }
        Ok(Self {
            directory,
            file,
            bytes,
        })
    }

    fn open(directory: &std::path::Path, exclusive: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).append(true);
        if exclusive {
            options.create_new(true);
        } else {
            options.create(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(directory.join(FILE_NAMES[0]))
    }

    fn write_record(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() as u64 > FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Oversized log record",
            ));
        }
        if self.bytes + bytes.len() as u64 > FILE_BYTES {
            self.file.flush()?;
            match fs::remove_file(self.directory.join(FILE_NAMES[2])) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::rename(
                self.directory.join(FILE_NAMES[1]),
                self.directory.join(FILE_NAMES[2]),
            ) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            fs::rename(
                self.directory.join(FILE_NAMES[0]),
                self.directory.join(FILE_NAMES[1]),
            )?;
            self.file = Self::open(&self.directory, true)?;
            self.bytes = 0;
        }
        self.file.write_all(bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_data()
    }
}

fn writer_loop(output: &Output, writer: io::Result<RotatingFile>) {
    let mut writer = match writer {
        Ok(writer) => writer,
        Err(error) => {
            output.fail("Log file initialization failed", &error);
            lock(&output.state).worker_finished = true;
            return;
        }
    };
    loop {
        let line = {
            let mut state = lock(&output.state);
            while state.file.is_empty() && state.accepting {
                state = output
                    .ready
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let line = state.file.pop_front();
            state.writing = line.is_some();
            line
        };
        let Some(line) = line else { break };
        if let Err(error) = writer.write_record(&line) {
            output.fail("Log file write failed", &error);
            lock(&output.state).worker_finished = true;
            return;
        }
        let mut state = lock(&output.state);
        state.file_written = state.file_written.saturating_add(1);
        state.writing = false;
    }
    if let Err(error) = writer.flush() {
        output.fail("Log file flush failed", &error);
    }
    lock(&output.state).worker_finished = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..100 {
                let path = std::env::temp_dir().join(format!(
                    "mado-logging-{}-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                ));
                if fs::create_dir(&path).is_ok() {
                    return Self(path);
                }
            }
            panic!("Cannot create test log directory");
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn emit(layer: &OutputLayer) {
        layer.emit("Rust", "INFO", None, "app.ready", "Ready", Value::Null);
    }

    #[test]
    fn queue_overflow_counters_are_independent() {
        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        for _ in 0..=FILE_QUEUE_CAPACITY {
            emit(&layer);
            assert_eq!(output.drain().entries.len(), 1);
        }
        let status = output.status();
        assert_eq!(status.gui_dropped, 0);
        assert_eq!(status.file_dropped, 1);
        assert_eq!(status.file_errors, 0);
        assert_eq!(status.file_pending, FILE_QUEUE_CAPACITY);

        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        for _ in 0..=GUI_QUEUE_CAPACITY {
            emit(&layer);
            lock(&output.state).file.pop_front();
        }
        let batch = output.drain();
        assert_eq!(batch.entries.len(), GUI_QUEUE_CAPACITY);
        assert_eq!(batch.entries[0].sequence, 2);
        assert_eq!(batch.gui_dropped, 1);
        assert_eq!(batch.file_dropped, 0);
        assert_eq!(output.drain().gui_dropped, 1);
    }

    #[test]
    fn script_and_rust_records_correlate_and_redact_before_both_outputs() {
        let directory = Directory::new();
        let logger = Logger::new(directory.0.clone()).unwrap();
        logger.emit(
            "Script",
            "INFO",
            Some("run-1"),
            "decision.selected",
            "ocr",
            json!({
                "selection": ["a", "b"], "count": 2, "nested": {"enabled": true},
                "password": "hidden-value", "ocr_text": "private-recognition",
                "screenshot": [1, 2, 3], "path": "/Users/private/game",
                "details": "failed at /Users/private/work/file.ts", "header": "Bearer abc123",
                "terminal_record": {"entry": "private-terminal"},
                "windows": "open C:\\Users\\private\\game",
                "payload": "{\"entry\":\"private-terminal\"}",
            }),
        );
        logger.with_dispatch(|| {
            tracing::info!(run = "run-1", code = "backend.ready", count = 7u64, "Ready");
        });
        logger.emit(
            "Script",
            "INFO",
            Some("run-1"),
            "workflow.step",
            "task-1",
            json!({"details": "(sk-alpha987)"}),
        );
        let entries = logger.drain().entries;
        let status = logger.shutdown();
        assert!(status.shutdown_complete);
        assert!(!status.shutdown_timed_out);
        assert_eq!(status.file_written, 3);
        assert_eq!(status.file_errors, 0);
        let lines = fs::read_to_string(directory.0.join(FILE_NAMES[0])).unwrap();
        let disk: Vec<Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let gui: Vec<Value> = entries
            .iter()
            .map(|entry| serde_json::to_value(entry).unwrap())
            .collect();
        assert_eq!(disk, gui);
        assert_eq!(disk[0]["message"], "ocr");
        assert_eq!(disk[2]["message"], "task-1");
        assert_eq!(disk[2]["fields"]["details"], REDACTED);
        assert_eq!(disk[0]["fields"]["selection"], json!(["a", "b"]));
        assert_eq!(disk[0]["fields"]["nested"]["enabled"], true);
        assert_eq!(disk[1]["source"], "Rust");
        assert_eq!(disk[1]["run"], "run-1");
        assert_eq!(disk[1]["fields"]["count"], 7);
        for secret in [
            "hidden-value",
            "private-recognition",
            "private/game",
            "private/work",
            "abc123",
            "private-terminal",
        ] {
            assert!(!lines.contains(secret));
        }
        for key in [
            "password",
            "ocr_text",
            "screenshot",
            "path",
            "details",
            "header",
            "terminal_record",
            "windows",
            "payload",
        ] {
            assert_eq!(disk[0]["fields"][key], REDACTED);
        }
    }

    #[test]
    fn oversized_and_deep_values_cannot_expand_sink_records() {
        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        let mut deep = json!({"password": "never-visible"});
        for _ in 0..100 {
            deep = json!({"nested": deep});
        }
        layer.emit(
            "Script",
            "INFO",
            None,
            "bounded.value",
            &"x".repeat(100_000),
            json!({
                "deep": deep,
                "many": vec!["y".repeat(10_000); 100],
            }),
        );
        let entry = output.drain().entries.pop().unwrap();
        assert_eq!(entry.message, TRUNCATED);
        let encoded = serde_json::to_vec(&entry).unwrap();
        assert!(encoded.len() < 64 * 1024);
        let text = String::from_utf8(encoded).unwrap();
        assert!(!text.contains("never-visible"));
    }

    #[test]
    fn arrays_at_depth_limit_still_have_a_bounded_record() {
        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        let mut fields = json!(vec![0; 100_000]);
        for _ in 0..5 {
            fields = json!([fields]);
        }
        layer.emit("Rust", "INFO", None, "bounded.depth", "Depth limit", fields);
        let entry = output.drain().entries.pop().unwrap();
        assert!(serde_json::to_vec(&entry).unwrap().len() < 64 * 1024);
    }

    #[test]
    fn partial_record_on_restart_preserves_evidence_and_refuses_file_sink() {
        let directory = Directory::new();
        let path = directory.0.join(FILE_NAMES[0]);
        let partial = b"{\"interrupted\":";
        fs::write(&path, partial).unwrap();
        let logger = Logger::new(directory.0.clone()).unwrap();
        logger.emit("Rust", "INFO", None, "app.ready", "Ready", Value::Null);
        let status = logger.shutdown();
        assert_eq!(status.file_errors, 1);
        assert_eq!(status.file_written, 0);
        assert_eq!(logger.drain().entries.len(), 1);
        assert_eq!(fs::read(path).unwrap(), partial);
    }

    #[test]
    fn actual_write_error_does_not_disable_gui_or_become_queue_overflow() {
        let directory = Directory::new();
        let path = directory.0.join(FILE_NAMES[0]);
        fs::write(&path, b"").unwrap();
        // A real read-only descriptor makes write_all fail on every supported OS.
        let writer = RotatingFile {
            directory: directory.0.clone(),
            file: File::open(path).unwrap(),
            bytes: 0,
        };
        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        emit(&layer);
        writer_loop(&output, Ok(writer));
        assert_eq!(output.status().file_dropped, 0);
        emit(&layer);
        let batch = output.drain();
        assert_eq!(batch.entries.len(), 2);
        assert_eq!(batch.file_errors, 1);
        assert_eq!(batch.file_dropped, 1);
        assert_eq!(batch.gui_dropped, 0);
        output.close();
        assert!(output.status().shutdown_complete);
    }

    #[test]
    fn file_initialization_failure_leaves_gui_available() {
        let directory = Directory::new();
        fs::create_dir(directory.0.join(FILE_NAMES[0])).unwrap();
        let logger = Logger::new(directory.0.clone()).unwrap();
        logger.emit("Rust", "WARN", None, "app.ready", "Ready", Value::Null);
        let status = logger.shutdown();
        assert_eq!(status.file_errors, 1);
        assert_eq!(status.file_dropped, 1);
        assert_eq!(logger.drain().entries.len(), 1);
    }

    #[test]
    fn rotation_keeps_three_size_bounded_jsonl_files() {
        let directory = Directory::new();
        let mut writer = RotatingFile::new(directory.0.clone()).unwrap();
        let mut record = serde_json::to_vec(&json!({"message": "x".repeat(1000)})).unwrap();
        record.push(b'\n');
        for _ in 0..4000 {
            writer.write_record(&record).unwrap();
        }
        writer.flush().unwrap();
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 3);
        for name in FILE_NAMES {
            let path = directory.0.join(name);
            assert!(fs::metadata(&path).unwrap().len() <= FILE_BYTES);
            for line in fs::read_to_string(path).unwrap().lines() {
                assert_eq!(
                    serde_json::from_str::<Value>(line).unwrap()["message"],
                    "x".repeat(1000)
                );
            }
        }
    }

    #[test]
    fn shutdown_reports_stalled_worker_without_waiting_indefinitely() {
        let output = Arc::new(Output::new());
        let layer = OutputLayer {
            output: Arc::clone(&output),
        };
        let dispatch = Dispatch::new(Registry::default().with(layer.clone()));
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);
        let worker_output = Arc::clone(&output);
        let thread = thread::spawn(move || {
            release_rx.recv().unwrap();
            lock(&worker_output.state).worker_finished = true;
            let _ = done_tx.send(());
            let _ = exited_tx.send(());
        });
        let logger = Logger {
            layer,
            dispatch,
            worker: Mutex::new(Some(WorkerGuard {
                thread: Some(thread),
                done: done_rx,
            })),
        };
        let start = Instant::now();
        let status = logger.shutdown();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(status.shutdown_timed_out);
        assert!(!status.shutdown_complete);
        logger.with_dispatch(|| tracing::info!("After shutdown"));
        assert!(logger.drain().entries.is_empty());
        assert_eq!(logger.status().gui_dropped, 1);
        release_tx.send(()).unwrap();
        exited_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(logger.shutdown().shutdown_complete);
    }
}
