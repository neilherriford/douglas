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

enum WriteState {
    Buffering {
        pending: VecDeque<String>,
        last_try: Instant,
    },
    Writing(BufWriter<File>),
}

pub struct BufferedFileReporter {
    path: PathBuf,
    state: Mutex<WriteState>,
    max_buffered: usize,
}

impl Reporter for BufferedFileReporter {
    fn emit(&self, event: Event) {
        let line = self.format_logfmt(&event);
        let mut state = self.state.lock().unwrap();
        match &mut *state {
            WriteState::Buffering { pending, last_try } => {
                if last_try.elapsed() > Duration::from_millis(500) {
                    *last_try = Instant::now();
                    if let Some(mut writer) = self.try_open() {
                        for old in pending.drain(..) {
                            let _ = writeln!(writer, "{old}");
                        }
                        let _ = writeln!(writer, "{line}");
                        let _ = writer.flush();
                        *state = WriteState::Writing(writer);
                        return;
                    }
                }
                if pending.len() >= self.max_buffered {
                    pending.pop_front();
                }
                pending.push_back(line);
            }
            WriteState::Writing(buffer) => {
                let _ = writeln!(buffer, "{line}");
                let _ = buffer.flush();
            }
        }
    }
}

impl BufferedFileReporter {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state: Mutex::new(WriteState::Buffering {
                pending: VecDeque::new(),
                last_try: Instant::now() - Duration::from_secs(60),
            }),
            max_buffered: 10_000,
        }
    }

    fn try_open(&self) -> Option<BufWriter<File>> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()
            .map(BufWriter::new)
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
