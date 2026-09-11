#[cfg(feature = "tui")]
mod tui_reporter;
#[cfg(feature = "tui")]
pub use tui_reporter::TuiReporter;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{
    cell::Cell,
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    os::fd::FromRawFd,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct ScopeId(Uuid);
impl ScopeId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ScopeId {
    fn default() -> Self {
        Self::new()
    }
}

impl Serialize for ScopeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for ScopeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        let uuid = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
        Ok(ScopeId(uuid))
    }
}

impl std::fmt::Display for ScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: SystemTime,
    pub scope_id: ScopeId,
    pub parent_id: Option<ScopeId>,
    pub kind: EventKind,
}

impl Event {
    pub fn start_scope(scope_id: ScopeId, label: &str, kind: ScopeKind) -> Self {
        Self::new(
            scope_id,
            EventKind::ScopeStarted {
                label: label.to_string(),
                kind,
            },
        )
    }

    pub fn end_scope(scope_id: ScopeId, label: &str, outcome: Outcome) -> Self {
        Self::new(
            scope_id,
            EventKind::ScopeEnded {
                label: label.to_string(),
                outcome,
            },
        )
    }

    pub fn new(scope_id: ScopeId, kind: EventKind) -> Self {
        Self {
            timestamp: SystemTime::now(),
            scope_id,
            parent_id: None,
            kind,
        }
    }

    pub fn new_child(scope_id: ScopeId, parent_id: ScopeId, label: &str, kind: ScopeKind) -> Self {
        Self {
            timestamp: SystemTime::now(),
            scope_id,
            parent_id: Some(parent_id),
            kind: EventKind::ScopeStarted {
                label: label.to_string(),
                kind,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum EventKind {
    ScopeStarted { label: String, kind: ScopeKind },
    ScopeEnded { label: String, outcome: Outcome },
    Progress { percent: u8 },
    Message { level: Level, text: String },
    PlanHint { steps: Vec<String> },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ScopeKind {
    Step,
    Phase,
    Task,
    Group,
}

impl std::fmt::Display for ScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScopeKind::Step => f.write_str("step"),
            ScopeKind::Phase => f.write_str("phase"),
            ScopeKind::Task => f.write_str("task"),
            ScopeKind::Group => f.write_str("group"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum Outcome {
    Ok,
    Failed,
    Skipped,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Ok => f.write_str("ok"),
            Outcome::Failed => f.write_str("failed"),
            Outcome::Skipped => f.write_str("skipped"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Level {
    Debug,
    Info,
    Warn,
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level::Debug => f.write_str("debug"),
            Level::Info => f.write_str("info"),
            Level::Warn => f.write_str("WARNING"),
        }
    }
}

pub struct ScopedReporter<'a> {
    reporter: &'a dyn Reporter,
    id: ScopeId,
    outcome: Cell<Outcome>,
    emit_on_drop: Cell<bool>,
    label: String,
}

impl<'a> ScopedReporter<'a> {
    pub fn new(reporter: &'a dyn Reporter, id: ScopeId, label: &str) -> Self {
        Self {
            reporter,
            id,
            outcome: Cell::new(Outcome::Ok),
            emit_on_drop: Cell::new(true), // armed — Drop emits if finish() is never called
            label: label.to_string(),
        }
    }

    pub fn id(&self) -> ScopeId {
        self.id
    }

    pub fn enter_scope(&self, label: &str, kind: ScopeKind) -> ScopedReporter<'_> {
        let id = ScopeId::new();
        self.reporter
            .emit(Event::new_child(id, self.id, label, kind));
        ScopedReporter::new(self.reporter, id, label)
    }

    pub fn message(&self, level: Level, text: &str) {
        self.reporter.emit(Event::new(
            self.id,
            EventKind::Message {
                level,
                text: text.to_string(),
            },
        ));
    }

    pub fn plan_hint(&self, steps: &[String]) {
        self.reporter.emit(Event::new(
            self.id,
            EventKind::PlanHint {
                steps: Vec::from(steps),
            },
        ));
    }

    pub fn progress(&self, value: u8) {
        self.reporter
            .emit(Event::new(self.id, EventKind::Progress { percent: value }));
    }

    pub fn finish(self, outcome: Outcome) {
        self.emit_on_drop.set(false); // disarm before emitting so Drop doesn't double-fire
        self.reporter.emit(Event::new(
            self.id,
            EventKind::ScopeEnded {
                outcome,
                label: self.label.clone(),
            },
        ));
    }
}

impl Drop for ScopedReporter<'_> {
    fn drop(&mut self) {
        if self.emit_on_drop.get() {
            self.reporter.emit(Event::new(
                self.id,
                EventKind::ScopeEnded {
                    outcome: self.outcome.get(),
                    label: self.label.clone(),
                },
            ));
        }
    }
}

pub trait Reporter: Send + Sync {
    fn emit(&self, event: Event);
}

impl Reporter for Arc<dyn Reporter> {
    fn emit(&self, event: Event) {
        self.as_ref().emit(event);
    }
}

pub struct ChannelReporter {
    sender: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl ChannelReporter {
    pub fn new(sender: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
        Self { sender }
    }
}

impl Reporter for ChannelReporter {
    fn emit(&self, event: Event) {
        let _ = self.sender.send(event);
    }
}

impl dyn Reporter {
    pub fn enter_scope(&self, label: &str, kind: ScopeKind) -> ScopedReporter<'_> {
        let id = ScopeId::new();
        self.emit(Event::new(
            id,
            EventKind::ScopeStarted {
                label: label.to_string(),
                kind,
            },
        ));
        ScopedReporter::new(self, id, label)
    }
}

#[derive(Clone)]
pub struct Span {
    pub reporter: Arc<dyn Reporter>,
    pub id: ScopeId,
    pub label: String,
}

impl Span {
    pub fn new(reporter: Arc<dyn Reporter>, label: &str, kind: ScopeKind) -> Self {
        let result = Self {
            reporter,
            id: ScopeId::new(),
            label: label.to_string(),
        };
        result
            .reporter
            .emit(Event::start_scope(result.id, label, kind));
        result
    }

    pub fn record(reporter: Arc<dyn Reporter>, label: &str, kind: ScopeKind, outcome: Outcome) {
        Self::new(reporter, label, kind)
            .start_guard()
            .finish_with_outcome(outcome);
    }

    pub fn create_child(&self, label: &str, kind: ScopeKind) -> Self {
        let child_scope_id = ScopeId::new();
        self.reporter
            .emit(Event::new_child(child_scope_id, self.id, label, kind));

        Self {
            id: child_scope_id,
            label: label.to_string(),
            reporter: Arc::clone(&self.reporter),
        }
    }

    pub fn message(&self, level: Level, text: &str) {
        self.reporter.emit(Event::new(
            self.id,
            EventKind::Message {
                level,
                text: text.to_string(),
            },
        ));
    }

    pub fn plan_hint(&self, steps: Vec<String>) {
        self.reporter
            .emit(Event::new(self.id, EventKind::PlanHint { steps }));
    }

    pub fn create_scoped_reporter(&self) -> ScopedReporter<'_> {
        ScopedReporter::new(self.reporter.as_ref(), self.id, &self.label)
    }

    pub fn start_guard(self) -> ScopeGuard {
        ScopeGuard {
            span: self,
            outcome: Outcome::Failed,
            closed: AtomicBool::new(false),
        }
    }
}

pub struct ScopeGuard {
    span: Span,
    outcome: Outcome,
    closed: AtomicBool,
}

impl ScopeGuard {
    pub fn reporter(&self) -> Arc<dyn Reporter> {
        Arc::clone(&self.span.reporter)
    }
    pub fn finish_with_outcome(&self, outcome: Outcome) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.span
                .reporter
                .emit(Event::end_scope(self.span.id, &self.span.label, outcome));
        }
    }

    pub fn finish<T, E>(&self, result: Result<T, E>) -> Result<T, E>
    where
        E: std::fmt::Display,
    {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let outcome = match &result {
                Ok(_) => Outcome::Ok,
                Err(e) => {
                    self.span.reporter.emit(Event::new(
                        self.span.id,
                        EventKind::Message {
                            level: Level::Warn,
                            text: e.to_string(),
                        },
                    ));
                    Outcome::Failed
                }
            };
            self.span
                .reporter
                .emit(Event::end_scope(self.span.id, &self.span.label, outcome));
        }
        result
    }

    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.span.reporter.emit(Event::end_scope(
                self.span.id,
                &self.span.label,
                self.outcome,
            ));
        }
    }
}

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROTATED_FILES: u32 = 5;

enum WriteState {
    Buffering {
        pending: VecDeque<String>,
        last_try: Instant,
    },
    Writing {
        writer: BufWriter<File>,
        bytes_written: u64,
    },
}

pub struct BufferedFileReporter {
    path: PathBuf,
    state: Mutex<WriteState>,
    max_buffered: usize,
    max_bytes: u64,
    max_rotated_files: u32,
    rotator: Arc<dyn file_system::FileRotator>,
}

impl Reporter for BufferedFileReporter {
    fn emit(&self, event: Event) {
        let line = self.format_logfmt(&event);
        let mut state = self.state.lock().unwrap();
        let current = std::mem::replace(
            &mut *state,
            WriteState::Buffering {
                pending: VecDeque::new(),
                last_try: Instant::now(),
            },
        );
        *state = self.write_line(current, line);
    }
}

impl BufferedFileReporter {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limits(
            path,
            MAX_LOG_BYTES,
            MAX_ROTATED_FILES,
            Arc::new(file_system::UnixFileRotator::new()),
        )
    }

    fn with_limits(
        path: impl Into<PathBuf>,
        max_bytes: u64,
        max_rotated_files: u32,
        rotator: Arc<dyn file_system::FileRotator>,
    ) -> Self {
        Self {
            path: path.into(),
            state: Mutex::new(WriteState::Buffering {
                pending: VecDeque::new(),
                last_try: Instant::now() - Duration::from_secs(60),
            }),
            max_buffered: 10_000,
            max_bytes,
            max_rotated_files,
            rotator,
        }
    }

    fn write_line(&self, state: WriteState, line: String) -> WriteState {
        match state {
            WriteState::Buffering {
                mut pending,
                mut last_try,
            } => {
                if last_try.elapsed() > Duration::from_millis(500) {
                    last_try = Instant::now();
                    if let Some((mut writer, mut bytes_written)) = self.try_open() {
                        for old in pending.drain(..) {
                            bytes_written += Self::write_and_count(&mut writer, &old);
                        }
                        bytes_written += Self::write_and_count(&mut writer, &line);
                        let _ = writer.flush();
                        return self.rotate_if_needed(writer, bytes_written);
                    }
                }
                if pending.len() >= self.max_buffered {
                    pending.pop_front();
                }
                pending.push_back(line);
                WriteState::Buffering { pending, last_try }
            }
            WriteState::Writing {
                mut writer,
                mut bytes_written,
            } => {
                bytes_written += Self::write_and_count(&mut writer, &line);
                let _ = writer.flush();
                self.rotate_if_needed(writer, bytes_written)
            }
        }
    }

    fn write_and_count(writer: &mut BufWriter<File>, line: &str) -> u64 {
        let _ = writeln!(writer, "{line}");
        (line.len() + 1) as u64
    }

    fn rotate_if_needed(&self, writer: BufWriter<File>, bytes_written: u64) -> WriteState {
        if bytes_written < self.max_bytes {
            return WriteState::Writing {
                writer,
                bytes_written,
            };
        }
        drop(writer);
        self.rotate();
        match self.try_open() {
            Some((writer, bytes_written)) => WriteState::Writing {
                writer,
                bytes_written,
            },
            None => WriteState::Buffering {
                pending: VecDeque::new(),
                last_try: Instant::now() - Duration::from_secs(60),
            },
        }
    }

    fn rotate(&self) {
        self.rotator.rotate(&self.path, self.max_rotated_files);
    }

    fn try_open(&self) -> Option<(BufWriter<File>, u64)> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()?;
        let bytes_written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Some((BufWriter::new(file), bytes_written))
    }

    fn format_logfmt(&self, event: &Event) -> String {
        let timestamp = humantime::format_rfc3339_seconds(event.timestamp);
        let mut result = format!("ts={timestamp}");
        match &event.kind {
            EventKind::ScopeStarted { label, kind } => {
                result.push_str(&format!(
                    " level=info event=scope.start scope={} label={label} kind={kind}",
                    event.scope_id
                ));

                if let Some(parent) = event.parent_id {
                    result.push_str(&format!(" parent={parent}"));
                }
            }
            EventKind::ScopeEnded { label, outcome } => {
                result.push_str(&format!(
                    " level=info event=scope.start scope={} label={label} outcome={outcome}",
                    event.scope_id
                ));

                if let Some(parent) = event.parent_id {
                    result.push_str(&format!(" parent={parent}"));
                }
            }
            EventKind::Progress { percent } => {
                result.push_str(&format!(
                    " level=debug event=progress scope={} complete={:02.2}",
                    event.scope_id,
                    (*percent as f32) / 255.0
                ));
            }
            EventKind::Message { level, text } => {
                result.push_str(&format!(
                    " level={level} event=message scope={} message={}",
                    event.scope_id,
                    escape_for_log(text),
                ));
            }
            EventKind::PlanHint { steps } => {
                result.push_str(&format!(
                    " level=debug event=plan_hint scope={} steps={}",
                    event.scope_id,
                    escape_for_log(
                        &steps
                            .iter()
                            .enumerate()
                            .map(|(index, step)| format!("{}. {step}", index + 1))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        };

        result
    }
}

fn escape_for_log(text: &str) -> String {
    if text
        .chars()
        .any(|char| char == ' ' || char == '"' || char == '=')
    {
        format!("\"{}\"", text.replace('\\', r"\\").replace('"', r#"\""#))
    } else {
        text.to_string()
    }
}

pub struct TeeReporter {
    sinks: Vec<Box<dyn Reporter>>,
}

impl TeeReporter {
    pub fn new(sinks: Vec<Box<dyn Reporter>>) -> Self {
        Self { sinks }
    }
}

impl Reporter for TeeReporter {
    fn emit(&self, event: Event) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
}

pub struct PipeReporter {
    writer: Mutex<BufWriter<File>>,
}

impl PipeReporter {
    /// # Safety
    ///
    /// `fd` must be a valid, open, writable file descriptor that will
    /// not be closed or duplicated by any other code for the lifetime
    /// of this `PipeReporter`. Ownership of the descriptor transfers
    /// to this type it will be closed when the `PipeReporter` is
    /// dropped.
    pub unsafe fn from_raw_fd(fd: i32) -> Self {
        let file = unsafe { File::from_raw_fd(fd) };
        Self {
            writer: Mutex::new(BufWriter::new(file)),
        }
    }
}

impl Reporter for PipeReporter {
    fn emit(&self, event: Event) {
        let Ok(mut w) = self.writer.lock() else {
            return;
        };
        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "douglas-log-test-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn message_event(text: &str) -> Event {
        Event::new(
            ScopeId::new(),
            EventKind::Message {
                level: Level::Info,
                text: text.to_string(),
            },
        )
    }

    fn emit_messages(reporter: &BufferedFileReporter, texts: &[&str]) {
        for text in texts {
            reporter.emit(message_event(text));
        }
    }

    fn read_to_string(path: &std::path::Path) -> String {
        let mut result = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut result)
            .unwrap();
        result
    }

    // The real UnixFileRotator, for tests exercising BufferedFileReporter's
    // actual end-to-end write-then-rotate behavior against real files —
    // whether rotate() itself got *called* at the right threshold is
    // covered separately below with a mock, with no filesystem involved.
    fn real_rotator() -> Arc<dyn file_system::FileRotator> {
        Arc::new(file_system::UnixFileRotator::new())
    }

    #[test]
    fn test_emit_should_not_rotate_when_under_the_size_limit() {
        let dir = temp_dir("under-limit");
        let path = dir.join("test.log");
        let reporter = BufferedFileReporter::with_limits(&path, 1024 * 1024, 5, real_rotator());

        emit_messages(&reporter, &["one", "two", "three"]);

        let contents = read_to_string(&path);
        assert!(contents.contains("one"));
        assert!(contents.contains("two"));
        assert!(contents.contains("three"));
        assert!(!dir.join("test.log.1").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_emit_should_rotate_the_previous_file_when_exceeding_the_size_limit() {
        let dir = temp_dir("rotate-once");
        let path = dir.join("test.log");
        let reporter = BufferedFileReporter::with_limits(&path, 150, 5, real_rotator());

        emit_messages(&reporter, &["first line"]);
        emit_messages(&reporter, &["second line"]);
        emit_messages(&reporter, &["third line"]);

        let rotated = read_to_string(&dir.join("test.log.1"));
        let current = read_to_string(&path);
        assert!(rotated.contains("first line"));
        assert!(rotated.contains("second line"));
        assert!(!rotated.contains("third line"));
        assert!(current.contains("third line"));
        assert!(!current.contains("first line"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rotate_should_cap_the_number_of_historical_files() {
        let dir = temp_dir("rotate-cap");
        let path = dir.join("test.log");
        let reporter = BufferedFileReporter::with_limits(&path, 10, 2, real_rotator());

        emit_messages(
            &reporter,
            &["line a", "line b", "line c", "line d", "line e"],
        );

        assert!(dir.join("test.log.1").exists());
        assert!(dir.join("test.log.2").exists());
        assert!(!dir.join("test.log.3").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_emit_should_account_for_an_already_large_existing_file() {
        let dir = temp_dir("existing-size");
        let path = dir.join("test.log");
        std::fs::write(&path, "x".repeat(100)).unwrap();
        let reporter = BufferedFileReporter::with_limits(&path, 10, 5, real_rotator());

        emit_messages(&reporter, &["triggers rotation"]);

        assert!(dir.join("test.log.1").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_emit_should_call_the_rotator_when_the_threshold_is_exceeded() {
        let dir = temp_dir("mock-rotate-called");
        let path = dir.join("test.log");
        let expected_path = path.clone();

        let mut mock_rotator = file_system::MockFileRotator::new();
        mock_rotator
            .expect_rotate()
            .withf(move |rotated_path, max_rotated_files| {
                rotated_path == expected_path && *max_rotated_files == 5
            })
            .times(1)
            .returning(|_, _| ());

        let reporter = BufferedFileReporter::with_limits(&path, 10, 5, Arc::new(mock_rotator));

        emit_messages(&reporter, &["long enough to cross a 10 byte threshold"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_emit_should_not_call_the_rotator_when_under_the_threshold() {
        let dir = temp_dir("mock-rotate-not-called");
        let path = dir.join("test.log");

        let mut mock_rotator = file_system::MockFileRotator::new();
        mock_rotator.expect_rotate().times(0);

        let reporter =
            BufferedFileReporter::with_limits(&path, 1024 * 1024, 5, Arc::new(mock_rotator));

        emit_messages(&reporter, &["short"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
