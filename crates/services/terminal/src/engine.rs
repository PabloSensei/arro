//! The thread that owns `Term`. Nobody else touches it: readers get snapshots, the reader thread
//! hands over chunks through a bounded channel, and commands arrive as messages. A second `vte`
//! parser sees each chunk first, because `Term` drops what a host has to act on.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use alacritty_terminal::vte::{Params, Parser, Perform};
use crossbeam_channel::{Receiver, Sender, never, select};

use crate::screen::{Cell, CellColor, CellFlags, Link, Modes, Screen, SearchResults};
use crate::search::{RowText, reveal_offset, search_rows};
use crate::session::{Control, SearchQuery, Shared, TerminalEvent};

/// How often a stream of output turns into a new picture.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(8);
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// Answers to color queries, the theme of the panel takes over when it exists.
const FOREGROUND: Rgb = Rgb {
    r: 0xBC,
    g: 0xBE,
    b: 0xC4,
};
const BACKGROUND: Rgb = Rgb {
    r: 0x1E,
    g: 0x1F,
    b: 0x22,
};

/// OSC codes a host can use: 7501 turn state, 9 and 777 notifications.
const FORWARDED_OSC: &[&[u8]] = &[b"7501", b"9", b"777"];

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// What `Term` reports through its listener, kept for the next snapshot.
struct Notes {
    title: String,
    bells: u64,
    size: Size,
}

struct Proxy {
    replies: Sender<Vec<u8>>,
    events: Sender<TerminalEvent>,
    notes: Rc<RefCell<Notes>>,
}

impl Proxy {
    /// A reply that does not fit is lost, which is better than a reader stuck behind the writer.
    fn reply(&self, text: String) {
        drop(self.replies.try_send(text.into_bytes()));
    }
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => self.reply(text),
            Event::ColorRequest(index, format) => {
                let color = if index == NamedColor::Background as usize {
                    BACKGROUND
                } else {
                    FOREGROUND
                };
                self.reply(format(color));
            }
            Event::TextAreaSizeRequest(format) => {
                let size = self.notes.borrow().size;
                self.reply(format(WindowSize {
                    num_lines: size.rows as u16,
                    num_cols: size.cols as u16,
                    cell_width: 8,
                    cell_height: 16,
                }));
            }
            Event::Title(title) => {
                self.notes.borrow_mut().title.clone_from(&title);
                drop(self.events.try_send(TerminalEvent::Title(title)));
            }
            Event::ResetTitle => self.notes.borrow_mut().title.clear(),
            Event::Bell => {
                self.notes.borrow_mut().bells += 1;
                drop(self.events.try_send(TerminalEvent::Bell));
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct Tap {
    win32_input: bool,
    osc: Vec<(String, String)>,
    cwd: Option<String>,
}

impl Perform for Tap {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private_mode = intermediates == b"?" && matches!(action, 'h' | 'l');
        if private_mode && params.iter().any(|param| param[0] == 9001) {
            self.win32_input = action == 'h';
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some((code, rest)) = params.split_first() else {
            return;
        };
        if let Some(cwd) = cwd_of(code, rest) {
            self.cwd = Some(cwd);
        }
        if FORWARDED_OSC.contains(code) {
            let payload: Vec<_> = rest
                .iter()
                .map(|part| String::from_utf8_lossy(part))
                .collect();
            let code = String::from_utf8_lossy(code).into_owned();
            self.osc.push((code, payload.join(";")));
        }
    }
}

/// The working directory an OSC names: OSC 7 carries a URI, and the OSC 9;9 of Windows
/// terminals carries the path itself.
fn cwd_of(code: &[u8], rest: &[&[u8]]) -> Option<String> {
    match code {
        b"7" => {
            let uri = String::from_utf8_lossy(rest.first()?).into_owned();
            (!uri.is_empty()).then_some(uri)
        }
        b"9" => {
            let (kind, path) = rest.split_first()?;
            if kind != b"9" {
                return None;
            }
            let path: Vec<_> = path
                .iter()
                .map(|part| String::from_utf8_lossy(part))
                .collect();
            let path = path.join(";");
            (!path.is_empty()).then_some(path)
        }
        _ => None,
    }
}

pub(crate) struct Engine {
    term: Term<Proxy>,
    parser: Processor,
    tap_parser: Parser,
    tap: Tap,
    notes: Rc<RefCell<Notes>>,
    shared: Arc<Shared>,
    events: Sender<TerminalEvent>,
    dirty: bool,
    last_publish: Instant,
    version: u64,
    closed: bool,
    /// The query the panel looks for, while the search bar is open.
    search: Option<SearchQuery>,
    search_generation: u64,
    /// Output arrived since the rows were last searched.
    search_dirty: bool,
    /// The latest answer of the search, for the view that scrolls to a match.
    results: SearchResults,
}

impl Engine {
    pub(crate) fn new(
        cols: u16,
        rows: u16,
        replies: Sender<Vec<u8>>,
        events: Sender<TerminalEvent>,
        shared: Arc<Shared>,
    ) -> Self {
        let size = Size {
            cols: cols as usize,
            rows: rows as usize,
        };
        let notes = Rc::new(RefCell::new(Notes {
            title: String::new(),
            bells: 0,
            size,
        }));
        let proxy = Proxy {
            replies,
            events: events.clone(),
            notes: notes.clone(),
        };
        Self {
            term: Term::new(Config::default(), &size, proxy),
            parser: Processor::new(),
            tap_parser: Parser::new(),
            tap: Tap::default(),
            notes,
            shared,
            events,
            dirty: true,
            last_publish: Instant::now(),
            version: 0,
            closed: false,
            search: None,
            search_generation: 0,
            search_dirty: false,
            results: SearchResults::default(),
        }
    }

    pub(crate) fn run(mut self, output: Receiver<Vec<u8>>, control: Receiver<Control>) {
        let mut control = control;
        loop {
            let wait = self.wait();
            select! {
                recv(output) -> chunk => match chunk {
                    Ok(chunk) => self.feed(&chunk),
                    Err(_) => break,
                },
                recv(control) -> message => match message {
                    Ok(message) => self.control(message),
                    Err(_) => control = never(),
                },
                default(wait) => {}
            }
            self.flush_expired_sync();
            if self.dirty && self.last_publish.elapsed() >= PUBLISH_INTERVAL {
                self.publish();
            }
        }
        self.flush_sync();
        // Close before the last publish: a panel that reads the new version must also
        // see the end, or a frame drawn between the two would never get another one.
        self.closed = true;
        self.publish();
        let code = self.shared.exit_code();
        drop(self.events.try_send(TerminalEvent::Exited(code)));
    }

    fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    fn wait(&self) -> Duration {
        let publish = self
            .dirty
            .then(|| PUBLISH_INTERVAL.saturating_sub(self.last_publish.elapsed()));
        let sync = self
            .sync_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let shortest = [publish, sync].into_iter().flatten().min();
        shortest.unwrap_or(IDLE_WAIT)
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.tap_parser.advance(&mut self.tap, chunk);
        self.parser.advance(&mut self.term, chunk);
        self.store_modes();
        for (code, payload) in self.tap.osc.drain(..) {
            drop(self.events.try_send(TerminalEvent::Osc { code, payload }));
        }
        self.shared
            .bytes
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        drop(self.events.try_send(TerminalEvent::Output));
        self.dirty = true;
        self.search_dirty = self.search.is_some();
    }

    /// A lost end-of-sync marker must not freeze the screen.
    fn flush_expired_sync(&mut self) {
        if self
            .sync_deadline()
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.flush_sync();
        }
    }

    fn flush_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.dirty = true;
    }

    fn modes(&self) -> Modes {
        let mode = *self.term.mode();
        Modes {
            win32_input: self.tap.win32_input,
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
            mouse: mode.intersects(TermMode::MOUSE_MODE),
        }
    }

    fn store_modes(&self) {
        let bits = self.modes().to_bits();
        self.shared.modes.store(bits, Ordering::SeqCst);
    }

    fn control(&mut self, message: Control) {
        match message {
            Control::Resize => self.resize(),
            Control::Scroll(delta) => {
                self.term.scroll_display(Scroll::Delta(delta));
                self.dirty = true;
            }
            Control::ScrollToBottom => {
                self.term.scroll_display(Scroll::Bottom);
                self.dirty = true;
            }
            Control::RevealMatch(index) => {
                self.update_search();
                let Some(line) = self.results.matches.get(index).map(|found| found.line) else {
                    return;
                };
                let offset = reveal_offset(line, self.term.grid().history_size());
                let current = self.term.grid().display_offset() as i32;
                self.term
                    .scroll_display(Scroll::Delta(offset as i32 - current));
                self.dirty = true;
            }
            Control::Search { query, generation } => {
                self.search = Some(query);
                self.search_generation = generation;
                self.search_dirty = true;
                self.results = SearchResults::default();
                // A quiet terminal publishes nothing of itself, and the answer of the
                // search would wait for the next output.
                self.dirty = true;
            }
            Control::ClearSearch => {
                self.search = None;
                self.search_dirty = false;
                self.results = SearchResults::default();
                self.shared.search.set(SearchResults::default());
            }
            Control::Clear => {
                // The terminal clears itself, as the clear command of a shell does:
                // the screen and the scrollback both go.
                self.parser.advance(&mut self.term, b"\x1b[H\x1b[2J\x1b[3J");
                self.dirty = true;
                self.search_dirty = self.search.is_some();
            }
        }
    }

    fn resize(&mut self) {
        let (cols, rows) = self.shared.requested_size();
        let size = Size {
            cols: cols as usize,
            rows: rows as usize,
        };
        let current = self.notes.borrow().size;
        if (current.cols, current.rows) == (size.cols, size.rows) {
            return;
        }
        drop(self.shared.pty.resize(cols, rows));
        self.notes.borrow_mut().size = size;
        self.term.resize(size);
        self.dirty = true;
    }

    fn publish(&mut self) {
        self.version += 1;
        self.shared.screen.set(self.screen());
        self.update_search();
        self.dirty = false;
        self.last_publish = Instant::now();
    }

    /// Searches the rows again when output arrived or a query is new, and hands the
    /// answer to the view.
    fn update_search(&mut self) {
        let Some(query) = &self.search else {
            return;
        };
        if !self.search_dirty {
            return;
        }
        let rows = self.row_texts();
        let results = search_rows(&rows, query, self.search_generation);
        self.results = results;
        self.shared.search.set(self.results.clone());
        self.search_dirty = false;
    }

    /// Every row of the history and of the screen, as text.
    fn row_texts(&self) -> Vec<RowText> {
        let (cols, rows) = {
            let size = self.notes.borrow().size;
            (size.cols, size.rows)
        };
        let grid = self.term.grid();
        let history = grid.history_size();
        let mut texts = Vec::with_capacity(history + rows);
        for line in -(history as i32)..rows as i32 {
            let row = &grid[alacritty_terminal::index::Line(line)];
            let cells = (0..cols as u16).filter_map(|column| {
                let cell = &row[alacritty_terminal::index::Column(column as usize)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    return None;
                }
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                Some((column, ch, cell.flags.contains(Flags::WIDE_CHAR)))
            });
            texts.push(RowText::new(line, cells));
        }
        texts
    }

    fn screen(&self) -> Screen {
        let size = self.notes.borrow().size;
        let (cols, rows) = (size.cols, size.rows);
        let mut cells = vec![Cell::default(); cols * rows];
        let mut links = Links::default();
        let content = self.term.renderable_content();
        let offset = content.display_offset;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset as i32;
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= rows || col >= cols {
                continue;
            }
            let row = row as usize;
            cells[row * cols + col] = convert(&indexed);
            if let Some(link) = indexed.hyperlink() {
                links.push(row as u16, col as u16, link.uri());
            }
        }
        let cursor = content.cursor.point;
        let on_screen = cursor.line.0 + offset as i32;
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR) && offset == 0;
        let notes = self.notes.borrow();
        Screen {
            cols: cols as u16,
            rows: rows as u16,
            cells,
            cursor: visible.then_some((on_screen as u16, cursor.column.0 as u16)),
            display_offset: offset,
            title: notes.title.clone(),
            modes: self.modes(),
            links: links.finished,
            bells: notes.bells,
            cwd: self.tap.cwd.clone(),
            version: self.version,
            exit_code: self.shared.exit.get().copied(),
            closed: self.closed,
        }
    }
}

/// Merges adjacent cells of one hyperlink.
#[derive(Default)]
struct Links {
    finished: Vec<Link>,
}

impl Links {
    fn push(&mut self, row: u16, col: u16, uri: &str) {
        if let Some(last) = self.finished.last_mut()
            && last.row == row
            && last.end_col == col
            && last.uri == uri
        {
            last.end_col = col + 1;
            return;
        }
        self.finished.push(Link {
            row,
            start_col: col,
            end_col: col + 1,
            uri: uri.to_string(),
        });
    }
}

fn convert_color(color: Color) -> CellColor {
    match color {
        Color::Spec(Rgb { r, g, b }) => CellColor::Rgb(r, g, b),
        Color::Indexed(index) => CellColor::Indexed(index),
        Color::Named(named) => match named as usize {
            index @ 0..=15 => CellColor::Indexed(index as u8),
            _ => CellColor::Default,
        },
    }
}

fn convert(cell: &TermCell) -> Cell {
    let mut flags = CellFlags::default();
    let pairs = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::ALL_UNDERLINES, CellFlags::UNDERLINE),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::DIM, CellFlags::DIM),
        (Flags::STRIKEOUT, CellFlags::STRIKEOUT),
        (Flags::WIDE_CHAR, CellFlags::WIDE),
        (Flags::WIDE_CHAR_SPACER, CellFlags::WIDE_SPACER),
    ];
    for (source, flag) in pairs {
        if cell.flags.intersects(source) {
            flags.insert(flag);
        }
    }
    Cell {
        ch: cell.c,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::cwd_of;

    #[test]
    fn the_working_directory_of_the_shell_is_read_from_its_escape() {
        assert_eq!(
            cwd_of(b"7", &[b"file://host/tmp/project"]).as_deref(),
            Some("file://host/tmp/project")
        );
        assert_eq!(
            cwd_of(b"9", &[b"9", b"C:\\project"]).as_deref(),
            Some("C:\\project")
        );
        assert_eq!(cwd_of(b"0", &[b"title"]), None, "a title is not a folder");
        assert_eq!(cwd_of(b"9", &[b"1", b"notice"]), None);
        assert_eq!(cwd_of(b"7", &[b""]), None);
    }
}
