use crossterm::{
    event::{Event as CtEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use log::{Event, EventKind, Level, Outcome, Reporter, ScopeId};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Stdout},
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
const MAX_LINES: usize = 200;

pub struct CliReporter {
    // Option so Drop can explicitly close the channel before joining.
    tx: Mutex<Option<mpsc::Sender<Event>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl CliReporter {
    pub fn start() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Event>();

        // Panic hook MUST be installed before we mangle the terminal.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore_term();
            prev(info);
        }));

        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

        let handle = thread::spawn(move || {
            render_loop(terminal, &rx); // handles cleanup itself
        });

        Ok(Self {
            tx: Mutex::new(Some(tx)),
            handle: Mutex::new(Some(handle)),
        })
    }
}

impl Reporter for CliReporter {
    fn emit(&self, event: Event) {
        let Ok(guard) = self.tx.lock() else { return };
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(event);
        }
    }
}

impl Drop for CliReporter {
    fn drop(&mut self) {
        // Drop the sender first — this closes the channel, which causes the
        // render loop to see TryRecvError::Disconnected and return. Without
        // this the join below would block forever because the loop never exits.
        if let Ok(mut guard) = self.tx.lock() {
            *guard = None;
        }

        let Ok(mut guard) = self.handle.lock() else {
            return;
        };
        if let Some(h) = guard.take() {
            let _ = h.join();
        }
    }
}

fn restore_term() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

struct ActiveScope {
    label: String,
}

#[derive(Default)]
struct AppState {
    scope_order: Vec<ScopeId>,
    scope_map: HashMap<ScopeId, ActiveScope>,
    lines: VecDeque<String>,
    spinner: u8,
}

impl AppState {
    fn apply(&mut self, event: Event) {
        match event.kind {
            EventKind::ScopeStarted { label, .. } => {
                self.scope_order.push(event.scope_id);
                self.scope_map.insert(event.scope_id, ActiveScope { label });
            }
            EventKind::ScopeEnded { label, outcome } => {
                self.scope_order.retain(|id| id != &event.scope_id);
                self.scope_map.remove(&event.scope_id);
                let sym = match outcome {
                    Outcome::Ok => "✅",
                    Outcome::Failed => "❌",
                    Outcome::Skipped => "⏭️",
                };
                self.push_line(format!("{sym} {label}"));
            }
            EventKind::Message { level, text } => {
                let prefix = match level {
                    Level::Debug | Level::Info => "ℹ️",
                    Level::Warn => "⚠️",
                };
                self.push_line(format!("{prefix}{text}"));
            }
            EventKind::PlanHint { steps } => {
                if steps.is_empty() {
                    self.push_line("Plan: nothing to do!".to_string());
                } else {
                    self.push_line("Plan:".to_string());
                    for (index, step) in steps.iter().enumerate() {
                        self.push_line(format!("  {}. {step}", index + 1));
                    }
                }
            }
            EventKind::Progress { .. } => {}
        }
    }

    fn push_line(&mut self, line: String) {
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

fn render_loop(mut terminal: Terminal<CrosstermBackend<Stdout>>, rx: &mpsc::Receiver<Event>) {
    let mut state = AppState::default();
    let tick_rate = Duration::from_millis(80);
    let mut last_tick = Instant::now();

    'running: loop {
        loop {
            match rx.try_recv() {
                Ok(ev) => state.apply(ev),
                Err(mpsc::TryRecvError::Empty) => break,
                // Channel closed — work is done. Break out to show the final frame.
                Err(mpsc::TryRecvError::Disconnected) => break 'running,
            }
        }

        if crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false)
            && let Ok(CtEvent::Key(k)) = crossterm::event::read()
            && (k.code == KeyCode::Char('q') || k.code == KeyCode::Enter)
        {
            break 'running;
        }

        if last_tick.elapsed() >= tick_rate {
            state.spinner = state.spinner.wrapping_add(1);
            last_tick = Instant::now();
        }

        let _ = terminal.draw(|f| draw(f, &state));
        thread::sleep(Duration::from_millis(16));
    }

    drop(terminal); // release the stdout handle before writing to it directly
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    for line in &state.lines {
        println!("{line}");
    }
}

fn draw(f: &mut Frame, state: &AppState) {
    let area = f.area();

    #[allow(clippy::cast_possible_truncation)]
    let running_height = (state.scope_order.len() as u16 + 2)
        .max(3)
        .min(area.height / 2);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(running_height), Constraint::Min(0)])
        .split(area);

    // Spinning indicator next to each active scope label.
    let spinner_char = SPINNER[usize::from(state.spinner) % SPINNER.len()];
    let scope_items: Vec<ListItem> = state
        .scope_order
        .iter()
        .filter_map(|id| state.scope_map.get(id))
        .map(|s| ListItem::new(format!("{spinner_char} {}", s.label)))
        .collect();

    f.render_widget(
        List::new(scope_items)
            .block(Block::default().borders(Borders::ALL).title(" Running "))
            .style(Style::default().fg(Color::Cyan)),
        chunks[0],
    );

    let visible_lines = chunks[1].height.saturating_sub(2).max(1) as usize;
    let log_items: Vec<ListItem> = state
        .lines
        .iter()
        .rev()
        .take(visible_lines)
        .rev()
        .map(|l| ListItem::new(l.as_str()))
        .collect();

    f.render_widget(
        List::new(log_items)
            .block(Block::default().borders(Borders::ALL).title(" Log "))
            .style(Style::default().fg(Color::White)),
        chunks[1],
    );
}
