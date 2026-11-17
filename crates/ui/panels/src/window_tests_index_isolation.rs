//! T-0218: the index of a project belongs to that project alone. A heavy project that
//! reads many files does not hold the index of a small one, and a small project answers
//! the palette in the frame that asked, without waiting for a thread of its own.
//!
//! The product keeps one project window per process, so the two indexes here are two
//! `ProjectIndex` instances: the same engine two windows of two processes would run.

use std::path::Path;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use keymap::workspace as keys;
use lang_api::{Symbol, SymbolKind, SymbolProvider};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::index::{ProjectIndex, SearchSetup};
use crate::palette::Palette;
use crate::window_tests::{
    active_path, draw, open_palette, press, scratch, type_text, wait_for_project_index,
};
use crate::window_tests_ui::{navigation_project, open_test_window};

/// A provider that does not answer until the test opens its gate. It is the heavy project:
/// its symbols are still read while the small project is expected to be ready.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    opened: Condvar,
}

#[derive(Default)]
struct GateState {
    entered: usize,
    open: bool,
}

impl Gate {
    fn enter(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.entered += 1;
        self.opened.notify_all();
    }

    fn wait(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while !state.open {
            state = self
                .opened
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    fn entered(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entered
    }

    fn open(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.open = true;
        self.opened.notify_all();
    }
}

static HEAVY_GATE: OnceLock<Gate> = OnceLock::new();

fn heavy_gate() -> &'static Gate {
    HEAVY_GATE.get_or_init(Gate::default)
}

fn declaration(path: &Path) -> Symbol {
    let name = path
        .file_stem()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    Symbol {
        name,
        kind: SymbolKind::Class,
        container: None,
        depth: 0,
        range: 0..0,
        name_range: 0..0,
    }
}

struct SlowSymbols;

impl SymbolProvider for SlowSymbols {
    fn file_symbols(&self, path: &Path, _: &str) -> Vec<Symbol> {
        heavy_gate().enter();
        heavy_gate().wait();
        vec![declaration(path)]
    }
}

struct FastSymbols;

impl SymbolProvider for FastSymbols {
    fn file_symbols(&self, path: &Path, _: &str) -> Vec<Symbol> {
        vec![declaration(path)]
    }
}

static SLOW: SlowSymbols = SlowSymbols;
static FAST: FastSymbols = FastSymbols;

fn slow_for(_: &Path) -> Option<&'static dyn SymbolProvider> {
    Some(&SLOW)
}

fn fast_for(_: &Path) -> Option<&'static dyn SymbolProvider> {
    Some(&FAST)
}

fn setup(symbols_for: fn(&Path) -> Option<&'static dyn SymbolProvider>) -> SearchSetup {
    SearchSetup {
        excludes: Vec::new(),
        symbols_for,
        extra_sources: Vec::new(),
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_heavy_index_does_not_hold_a_small_one() {
    let heavy_root = scratch("index-isolation-heavy");
    for at in 0..4 {
        std::fs::write(heavy_root.join(format!("Heavy{at}.slow")), "class Heavy\n").unwrap();
    }
    let small_root = scratch("index-isolation-small");
    std::fs::write(small_root.join("Small.src"), "class Small\n").unwrap();

    let (heavy, _heavy_events) = ProjectIndex::start(heavy_root, Vec::new(), setup(slow_for));
    wait_until("the heavy project to start reading", || {
        heavy.files_generation() > 0 && heavy_gate().entered() > 0
    });

    let (small, _small_events) = ProjectIndex::start(small_root, Vec::new(), setup(fast_for));
    let started = Instant::now();
    wait_until("the small project index", || small.symbols_generation() > 0);
    let small_ready = started.elapsed();

    assert_eq!(
        heavy.symbols_generation(),
        0,
        "the heavy project is still inside its provider"
    );
    assert!(
        small_ready < Duration::from_secs(2),
        "the small index waited for the heavy one: {small_ready:?}"
    );

    heavy_gate().open();
    wait_until("the heavy project index", || heavy.symbols_generation() > 0);
}

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

#[ui_kit::gpui::test]
fn a_small_project_answers_its_class_on_the_first_enter(cx: &mut TestAppContext) {
    let root = navigation_project("index-isolation-palette");
    let sample = root.join("src").join("Sample.txt");
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_index(cx);

    open_palette(cx, window, keys::GOTO_CLASS);
    type_text(cx, window, "Owner");
    draw(cx, window);

    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.iter().any(|(row, _)| row == "Owner"),
        "a small index answers in the frame that asked: {rows:?}"
    );

    press(cx, window, "enter");
    assert_eq!(
        active_path(cx, window),
        Some(sample),
        "a small project opens its class on the first Enter"
    );
}
