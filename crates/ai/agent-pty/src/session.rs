//! A worker per session turns terminal events into agent events and carries queued prompts to the
//! input box. It sleeps and waits so that no caller has to.

use std::collections::VecDeque;
use std::env::temp_dir;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use agent_core::{AgentError, AgentEvent, AgentSession, Harness, SessionStatus, Surface};
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use terminal::{Key, KeyEvent, Mods, TerminalEvent, TerminalSession};

use crate::gate::input_box_visible;

const TICK: Duration = Duration::from_millis(100);
/// Silence on screen that counts as the end of drawing. TUIs animate while they work.
const SETTLED: Duration = Duration::from_millis(400);
const TURN_QUIET: Duration = Duration::from_millis(1500);
/// Output that goes on this long is an agent at work and not a user typing.
const BUSY_AFTER: Duration = Duration::from_secs(2);
/// A harness needs this long to reach its input box on a cold start.
pub(crate) const FIRST_PROMPT_WAIT: Duration = Duration::from_secs(40);
const NEXT_PROMPT_WAIT: Duration = Duration::from_secs(5);
/// Codex takes a key right after a fast burst as part of a paste.
const CODEX_ENTER_DELAY: Duration = Duration::from_millis(1500);
const CLAUDE_ENTER_DELAY: Duration = Duration::from_millis(300);
/// Codex reads a paste key by key. Past this size the text goes through a file.
const CODEX_PROMPT_LIMIT: usize = 16 * 1024;
const QUEUE: usize = 64;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Rules {
    pub(crate) harness: Harness,
    pub(crate) marker: Option<String>,
    pub(crate) prompts: bool,
    pub(crate) first_prompt_wait: Duration,
}

pub(crate) struct PtySession {
    terminal: Arc<TerminalSession>,
    stream: Receiver<AgentEvent>,
    commands: Sender<String>,
    accepts_prompts: bool,
}

impl PtySession {
    pub(crate) fn start(terminal: Arc<TerminalSession>, rules: Rules) -> Result<Self, AgentError> {
        let (events, stream) = unbounded();
        let (commands, inbox) = bounded(QUEUE);
        let accepts_prompts = rules.prompts;
        let worker = Worker::new(terminal.clone(), rules, events);
        thread::Builder::new()
            .name("agent-pty".to_string())
            .spawn(move || worker.run(inbox))?;
        Ok(Self {
            terminal,
            stream,
            commands,
            accepts_prompts,
        })
    }
}

impl AgentSession for PtySession {
    fn surface(&self) -> Surface {
        Surface::Terminal(self.terminal.clone())
    }

    fn events(&self) -> Receiver<AgentEvent> {
        self.stream.clone()
    }

    fn prompt(&self, text: &str) -> Result<(), AgentError> {
        if !self.accepts_prompts {
            return Err(AgentError::Unsupported("prompts for this harness"));
        }
        self.commands
            .try_send(text.to_string())
            .map_err(|_| AgentError::Busy)
    }

    /// Both harnesses stop a turn on Escape, and it does nothing harmful in an idle box.
    fn cancel(&self) -> Result<(), AgentError> {
        self.terminal
            .key(KeyEvent::plain(Key::Escape))
            .map_err(|_| AgentError::Busy)
    }

    fn close(&self) {
        self.terminal.terminate();
    }
}

struct Pending {
    text: String,
    queued: Instant,
}

struct Worker {
    terminal: Arc<TerminalSession>,
    rules: Rules,
    events: Sender<AgentEvent>,
    status: SessionStatus,
    pending: VecDeque<Pending>,
    /// Waiting for the first prompt ever typed: the check is stricter then.
    first: bool,
    in_turn: bool,
    last_output: Instant,
    bytes: u64,
    /// When the current run of output began.
    burst: Option<Instant>,
    /// For prompts too long to type. Removed with the session.
    directory: Option<PathBuf>,
}

impl Worker {
    fn new(terminal: Arc<TerminalSession>, rules: Rules, events: Sender<AgentEvent>) -> Self {
        Self {
            terminal,
            rules,
            events,
            status: SessionStatus::Starting,
            pending: VecDeque::new(),
            first: true,
            in_turn: false,
            last_output: Instant::now(),
            bytes: 0,
            burst: None,
            directory: None,
        }
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn set_status(&mut self, status: SessionStatus) {
        if self.status != status {
            self.status = status;
            self.emit(AgentEvent::Status(status));
        }
    }

    fn run(mut self, inbox: Receiver<String>) {
        let terminal_events = self.terminal.events();
        let mut inbox = inbox;
        self.emit(AgentEvent::Status(self.status));
        loop {
            select! {
                recv(terminal_events) -> event => match event {
                    Ok(TerminalEvent::Exited(code)) => {
                        self.finish(code);
                        return;
                    }
                    Ok(event) => self.on_terminal(event),
                    Err(_) => {
                        self.finish(None);
                        return;
                    }
                },
                recv(inbox) -> text => match text {
                    Ok(text) => self.pending.push_back(Pending {
                        text,
                        queued: Instant::now(),
                    }),
                    Err(_) => inbox = crossbeam_channel::never(),
                },
                default(TICK) => {}
            }
            self.tick();
        }
    }

    fn on_terminal(&self, event: TerminalEvent) {
        match event {
            TerminalEvent::Title(title) => self.emit(AgentEvent::Title(title)),
            TerminalEvent::Bell => self.emit(AgentEvent::Notification("bell".to_string())),
            TerminalEvent::Osc { code, payload } => {
                self.emit(AgentEvent::Notification(format!("osc {code} {payload}")));
            }
            TerminalEvent::Output | TerminalEvent::Exited(_) => {}
        }
    }

    fn box_visible(&self, first: bool) -> bool {
        let lines = self.terminal.screen().lines();
        let marker = self.rules.marker.as_deref();
        input_box_visible(self.rules.harness, &lines, first, marker)
    }

    fn tick(&mut self) {
        let bytes = self.terminal.output_bytes();
        if bytes != self.bytes {
            self.bytes = bytes;
            self.last_output = Instant::now();
            self.burst.get_or_insert(self.last_output);
        }
        let quiet = self.last_output.elapsed();
        self.track_burst(quiet);
        if self.in_turn {
            if quiet >= TURN_QUIET {
                self.in_turn = false;
                self.emit(AgentEvent::TurnEnded);
                self.set_status(SessionStatus::Done);
            }
            return;
        }
        let settled = quiet >= SETTLED;
        if settled && self.pending.is_empty() {
            let status = match self.status {
                SessionStatus::Starting => Some(SessionStatus::WaitingForUser),
                _ => None,
            };
            status
                .into_iter()
                .for_each(|status| self.set_status(status));
            return;
        }
        let Some(front) = self.pending.front() else {
            return;
        };
        let visible = settled && self.box_visible(self.first);
        let limit = if self.first {
            self.rules.first_prompt_wait
        } else {
            NEXT_PROMPT_WAIT
        };
        if visible {
            let text = self.pending.pop_front().expect("front exists").text;
            self.first = false;
            self.send_prompt(&text);
        } else if front.queued.elapsed() >= limit {
            self.pending.pop_front();
            let message = "the input box is not on screen, the prompt was not sent";
            self.emit(AgentEvent::Error(message.to_string()));
            self.set_status(SessionStatus::WaitingForUser);
        }
    }

    fn track_burst(&mut self, quiet: Duration) {
        let Some(start) = self.burst else {
            return;
        };
        if quiet >= TURN_QUIET {
            self.burst = None;
            self.emit(AgentEvent::Idle);
            if !self.in_turn && self.status == SessionStatus::Running {
                self.set_status(SessionStatus::WaitingForUser);
            }
        } else if !self.in_turn
            && self.status != SessionStatus::Starting
            && start.elapsed() >= BUSY_AFTER
        {
            self.set_status(SessionStatus::Running);
        }
    }

    fn enter_delay(&self) -> Duration {
        match self.rules.harness {
            Harness::Codex => CODEX_ENTER_DELAY,
            _ => CLAUDE_ENTER_DELAY,
        }
    }

    fn prepare_text(&mut self, text: &str) -> std::io::Result<String> {
        if self.rules.harness != Harness::Codex || text.len() <= CODEX_PROMPT_LIMIT {
            return Ok(text.to_string());
        }
        let directory = temp_dir().join(format!(
            "ide-agent-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory)?;
        let file = directory.join("prompt.md");
        fs::write(&file, text)?;
        self.directory = Some(directory);
        Ok(format!("Follow the prompt in the file {}", file.display()))
    }

    fn send_prompt(&mut self, text: &str) {
        let text = match self.prepare_text(text) {
            Ok(text) => text,
            Err(error) => {
                self.emit(AgentEvent::Error(format!(
                    "the prompt was not sent: {error}"
                )));
                return;
            }
        };
        // Codex takes any CR as a submit, so its new lines have to be keys.
        let typed = match self.rules.harness {
            Harness::Codex => self.terminal.paste_lines(&text),
            _ => self.terminal.paste(&text),
        };
        if typed.is_err() {
            self.emit(AgentEvent::Error("the terminal took no input".to_string()));
            return;
        }
        thread::sleep(self.enter_delay());
        // The screen may have changed under the text, a dialog takes Enter for an answer.
        if !self.box_visible(false) {
            let message = "the input box went away, Enter was not pressed";
            self.emit(AgentEvent::Error(message.to_string()));
            self.set_status(SessionStatus::WaitingForUser);
            return;
        }
        let enter = KeyEvent::new(Key::Enter, Mods::default());
        if self.terminal.key(enter).is_err() {
            self.emit(AgentEvent::Error("the terminal took no input".to_string()));
            return;
        }
        self.in_turn = true;
        self.last_output = Instant::now();
        self.emit(AgentEvent::TurnStarted);
        self.set_status(SessionStatus::Running);
    }

    fn finish(&mut self, code: Option<u32>) {
        if self.in_turn {
            self.emit(AgentEvent::TurnEnded);
        }
        let status = match code {
            Some(0) | None => SessionStatus::Done,
            Some(_) => SessionStatus::Error,
        };
        self.set_status(status);
        self.emit(AgentEvent::Exited(code));
        if let Some(directory) = self.directory.take() {
            let _ = fs::remove_dir_all(directory);
        }
    }
}
