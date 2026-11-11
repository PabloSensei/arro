//! The rest of the window without a real window: search and navigation from the project
//! index, the Find in Files and Replace in Files windows, live templates, the editor view
//! switches, the status bar, reload and autosave, the tool windows without their services,
//! Git branches, Local History of a folder and the start screen.
//!
//! The project of these tests is a plain text one: a small symbol provider reads the
//! declarations out of `class`, `method` and `field` lines, so that no language server and
//! no real language is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use editor_view::{
    EditorView, FoldStyle, LanguageConfig, Languages, StatementRules, TEMPLATE_ANY, Template,
    TemplateKind, TemplateLanguage,
};
use keymap::{editor as editor_keys, workspace as keys};
use lang_api::{Symbol, SymbolKind, SymbolProvider};
use ui_kit::gpui;
use ui_kit::gpui::{
    ModifiersChangedEvent, MouseButton, ScrollDelta, ScrollEvent, TestAppContext,
    VisualTestContext, WindowHandle,
};
use ui_kit::{Modifiers, Theme, point, px};
use workspace::{
    FileIcons, Launch, Layout, SERVICES_TOOL, Settings, TERMINAL_TOOL, TODO_TOOL, Workspace,
};

use crate::docker::ActiveDocker;
use crate::git::ActiveGit;
use crate::index::SearchSetup;
use crate::plugins::ActivePlugins;
use crate::start::{ProjectTemplate, StartScreen};
use crate::structure::ActiveStructure;
use crate::terminal_panel::ActiveTerminal;
use crate::todo::ActiveTodo;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, open_palette, open_path, press,
    project_setup, sample_project, scratch, type_text, wait_for_project_index,
};
use crate::{AgentOptions, LanguageServer, connect, install};

/// How long a test waits for the index thread, a background save or a git call.
const LIMIT: Duration = Duration::from_secs(20);
/// One turn of the simulated clock.
const TICK: Duration = Duration::from_millis(50);

/// The environment variables the windows read at creation are process wide.
pub(crate) static ENV: Mutex<()> = Mutex::new(());

/// Runs `check` until it holds, letting the real threads of the window work.
pub(crate) fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
}

/// The declarations of the test language: a keyword at the start of a line names a
/// declaration, the indentation gives its depth and the block above it its container.
struct SampleSymbols;

const KEYWORDS: [(&str, SymbolKind); 6] = [
    ("class ", SymbolKind::Class),
    ("interface ", SymbolKind::Interface),
    ("enum ", SymbolKind::Enum),
    ("method ", SymbolKind::Method),
    ("field ", SymbolKind::Field),
    ("constant ", SymbolKind::Constant),
];

impl SymbolProvider for SampleSymbols {
    fn file_symbols(&self, _: &Path, text: &str) -> Vec<Symbol> {
        struct Line {
            start: usize,
            end: usize,
            depth: u32,
            name: usize,
        }

        let mut lines = Vec::new();
        let mut found: Vec<Symbol> = Vec::new();
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let content = line.trim_end_matches(['\r', '\n']);
            let indent = content.len() - content.trim_start().len();
            let depth = (indent / 4) as u32;
            let trimmed = content.trim_start();
            if let Some((keyword, kind)) = KEYWORDS
                .iter()
                .find(|(keyword, _)| trimmed.starts_with(keyword))
            {
                let rest = &trimmed[keyword.len()..];
                let name = rest
                    .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                    .next()
                    .unwrap_or_default();
                if !name.is_empty() {
                    let name_start = offset + indent + keyword.len();
                    found.push(Symbol {
                        name: name.to_owned(),
                        kind: *kind,
                        container: None,
                        depth,
                        range: offset..offset + content.len(),
                        name_range: name_start..name_start + name.len(),
                    });
                    lines.push(Line {
                        start: offset,
                        end: offset + content.len(),
                        depth,
                        name: found.len() - 1,
                    });
                }
            }
            offset += line.len();
        }

        for symbol in &mut found {
            if !matches!(
                symbol.kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Enum
                    | SymbolKind::Function
                    | SymbolKind::Method
                    | SymbolKind::Constructor
            ) {
                continue;
            }
            let start = symbol.range.start;
            let depth = symbol.depth;
            let next = lines
                .iter()
                .find(|line| line.start > start && line.depth <= depth);
            symbol.range.end = next.map_or(offset, |line| line.start);
        }

        // The container is the nearest declaration above with a smaller depth.
        let starts: Vec<(usize, usize, u32, String)> = lines
            .iter()
            .map(|line| {
                (
                    line.start,
                    line.end,
                    line.depth,
                    found[line.name].name.clone(),
                )
            })
            .collect();
        for symbol in &mut found {
            let start = symbol.range.start;
            let depth = symbol.depth;
            symbol.container = starts
                .iter()
                .filter(|(other, _, other_depth, _)| *other < start && *other_depth < depth)
                .max_by_key(|(other, _, _, _)| *other)
                .map(|(_, _, _, name)| name.clone());
        }
        found
    }
}

static SYMBOLS: SampleSymbols = SampleSymbols;

fn symbols_for(path: &Path) -> Option<&'static dyn SymbolProvider> {
    let extension = path.extension()?.to_str()?;
    extension
        .eq_ignore_ascii_case("txt")
        .then_some(&SYMBOLS as &'static dyn SymbolProvider)
}

static FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "\u{2026}",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

static STATEMENTS: StatementRules = StatementRules {
    terminator: ";",
    block_words: &["if", "for", "while", "class"],
};

static TEMPLATE_CONTEXTS: &[&str] = &[TEMPLATE_ANY];

static TEMPLATES: &[Template] = &[
    Template {
        key: "sout",
        description: "Prints a value",
        kind: TemplateKind::Abbreviation,
        body: "print($END$);",
        fields: &[],
        contexts: TEMPLATE_CONTEXTS,
    },
    Template {
        key: "psvm",
        description: "A main declaration",
        kind: TemplateKind::Abbreviation,
        body: "void main() {\n\t$END$\n}",
        fields: &[],
        contexts: TEMPLATE_CONTEXTS,
    },
    Template {
        key: "fori",
        description: "A counted loop",
        kind: TemplateKind::Abbreviation,
        body: "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$END$\n}",
        fields: &[("INDEX", "i"), ("LIMIT", "n")],
        contexts: TEMPLATE_CONTEXTS,
    },
    Template {
        key: "var",
        description: "Introduces a local variable",
        kind: TemplateKind::Postfix,
        body: "var $NAME$ = $EXPR$;$END$",
        fields: &[("NAME", "value")],
        contexts: TEMPLATE_CONTEXTS,
    },
    Template {
        key: "if",
        description: "if",
        kind: TemplateKind::Surround,
        body: "if (true) {\n\t$SELECTION$\n}$END$",
        fields: &[],
        contexts: TEMPLATE_CONTEXTS,
    },
];

/// The templates of the test language.
fn test_language() -> &'static TemplateLanguage {
    static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
    LANGUAGE.get_or_init(|| TemplateLanguage {
        templates: TEMPLATES,
        contexts: "",
        grammar: || dockerfile_syntax::config().grammar,
        indent_unit: "    ",
        receiver_prefixes: &["new"],
        query: OnceLock::new(),
    })
}

/// The language of a `.txt` file of the tests: the Dockerfile grammar for a tree, the
/// folding, statement and template rules of the test.
fn resolve(path: &Path) -> Option<LanguageConfig> {
    if dockerfile_syntax::is_dockerfile(path) {
        return Some(dockerfile_syntax::config());
    }
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.folds = "(source_file) @fold";
    config.fold_styles = FOLD_STYLES;
    config.statements = Some(STATEMENTS);
    config.line_comment = Some("#");
    config.indent_unit = "    ";
    Some(config)
}

fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    let extension = path.extension()?.to_str()?;
    extension.eq_ignore_ascii_case("txt").then(test_language)
}

fn search_setup() -> SearchSetup {
    SearchSetup {
        excludes: Vec::new(),
        symbols_for,
        extra_sources: Vec::new(),
    }
}

/// The window of these tests: the same order as `panels::run`, with the test language and
/// the test symbol index instead of the ones of an application.
pub(crate) fn open_test_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve,
            templates: templates_for,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            search_setup(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn open_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_test_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    window
}

const SAMPLE: &str = "class Owner\n    method getOwner\n        body one\n        body two\nclass Pet\n    method feed\n        body three\n";

pub(crate) fn navigation_project(name: &str) -> PathBuf {
    let root = sample_project(name);
    fs::write(root.join("src").join("Sample.txt"), SAMPLE).unwrap();
    root
}

fn long_project(name: &str) -> PathBuf {
    let root = sample_project(name);
    let mut text = String::from("class Long\n    method walk\n");
    for line in 0..120 {
        text.push_str(&format!("        body {line}\n"));
    }
    fs::write(root.join("src").join("Long.txt"), text).unwrap();
    root
}

/// Opens the palette until the index knows the class `Owner`, which means the files and
/// the symbols of the project are there.
pub(crate) fn wait_for_symbols(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) {
    wait_for(cx, "the project index", |cx| {
        open_palette(cx, window, keys::GOTO_CLASS);
        type_text(cx, window, "Owner");
        draw(cx, window);
        press(cx, window, "enter");
        let opened = active_path(cx, window).as_deref() == Some(path);
        if !opened {
            press(cx, window, "escape");
        }
        opened
    });
}

pub(crate) fn wait_for_files(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    wait_for(cx, "the file index", |cx| {
        open_palette(cx, window, keys::GOTO_FILE);
        type_text(cx, window, &name);
        draw(cx, window);
        press(cx, window, "enter");
        let opened = active_path(cx, window).as_deref() == Some(path);
        if !opened {
            press(cx, window, "escape");
        }
        opened
    });
}

pub(crate) fn wait_for_outline(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) {
    let view = editor_of(cx, window, path);
    wait_for(cx, "the outline", |cx| {
        view.read_with(cx, |view, _| !view.outline().is_empty())
    });
}

/// Whether a popup of the palette is on screen.
fn palette_open(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds("palette")
        .is_some()
}

pub(crate) fn double_shift(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let change = |modifiers| ModifiersChangedEvent {
        modifiers,
        ..ModifiersChangedEvent::default()
    };
    visual.simulate_event(change(shift));
    visual.simulate_event(change(Modifiers::default()));
    visual.simulate_event(change(shift));
    visual.simulate_event(change(Modifiers::default()));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn search_everywhere_and_the_goto_windows_reach_the_index(cx: &mut TestAppContext) {
    let root = navigation_project("search-ui");
    let window = open_window(cx, root.clone());
    let sample = root.join("src").join("Sample.txt");
    wait_for_project_index(cx);
    wait_for_symbols(cx, window, &sample);
    let view = editor_of(cx, window, &sample);

    // Double Shift opens Search Everywhere; the class of the index is picked. The
    // sources answer in their own threads, so a row can come after the first frame:
    // the place of the caret is what says that the row arrived and was opened.
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    wait_for(cx, "Search Everywhere to open the class", |cx| {
        if palette_open(cx, window) {
            press(cx, window, "escape");
        }
        double_shift(cx, window);
        draw(cx, window);
        type_text(cx, window, "Owner");
        draw(cx, window);
        press(cx, window, "enter");
        let at = view.read_with(cx, |view, _| view.caret());
        if at != (0, 6) {
            press(cx, window, "escape");
        }
        at == (0, 6)
    });
    assert_eq!(active_path(cx, window), Some(sample.clone()));
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 6));

    // Ctrl+Shift+N is Go to File.
    press(cx, window, "ctrl-shift-n");
    draw(cx, window);
    type_text(cx, window, "Sample");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(sample.clone()));

    // Ctrl+N is Go to Class.
    wait_for(cx, "Go to Class to open the class", |cx| {
        if palette_open(cx, window) {
            press(cx, window, "escape");
        }
        press(cx, window, "ctrl-n");
        draw(cx, window);
        type_text(cx, window, "Pet");
        draw(cx, window);
        press(cx, window, "enter");
        let at = view.read_with(cx, |view, _| view.caret());
        if at != (4, 6) {
            press(cx, window, "escape");
        }
        at == (4, 6)
    });
    assert_eq!(active_path(cx, window), Some(sample.clone()));

    // Ctrl+Alt+Shift+N is Go to Symbol.
    wait_for(cx, "Go to Symbol to open the method", |cx| {
        if palette_open(cx, window) {
            press(cx, window, "escape");
        }
        press(cx, window, "ctrl-alt-shift-n");
        draw(cx, window);
        type_text(cx, window, "feed");
        draw(cx, window);
        press(cx, window, "enter");
        let at = view.read_with(cx, |view, _| view.caret());
        if at != (5, 11) {
            press(cx, window, "escape");
        }
        at == (5, 11)
    });
    assert_eq!(active_path(cx, window), Some(sample.clone()));

    // Ctrl+F12 is the structure of the open file; its first row is the class.
    press(cx, window, "ctrl-f12");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "palette").size.height > px(0.),
        "Ctrl+F12 opens the file structure"
    );
    press(cx, window, "enter");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 6));

    // Alt+7 is the Structure tool window.
    press(cx, window, "alt-7");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace
                .tool_open(workspace::STRUCTURE_TOOL))
            .unwrap(),
        "Alt+7 opens Structure"
    );
    wait_for_outline(cx, window, &sample);
    let structure = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    let lines = structure.read_with(cx, |panel, cx| panel.lines(cx));
    assert!(lines.iter().any(|line| line == "Owner"), "{lines:?}");
    assert!(lines.iter().any(|line| line == "  getOwner"), "{lines:?}");
    press(cx, window, "alt-7");
}

#[ui_kit::gpui::test]
fn recent_files_locations_and_breadcrumbs_work(cx: &mut TestAppContext) {
    let root = navigation_project("recent-ui");
    let window = open_window(cx, root.clone());
    let sample = root.join("src").join("Sample.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    wait_for_symbols(cx, window, &sample);
    wait_for_outline(cx, window, &sample);

    // A place of an edit is in Recent Locations.
    let view = editor_of(cx, window, &sample);
    view.update(cx, |view, cx| view.restore_caret(2, 0, cx));
    type_text(cx, window, "z");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "ctrl-shift-e");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "places-popup").size.height > px(0.),
        "Ctrl+Shift+E lists the places"
    );
    press(cx, window, "enter");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 0));

    // Ctrl+E goes back to the file used before the current one.
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);
    press(cx, window, "ctrl-e");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    // The breadcrumbs of the editor name the declarations around the caret.
    open_path(cx, window, &sample);
    wait_for_outline(cx, window, &sample);
    view.update(cx, |view, cx| view.restore_caret(2, 0, cx));
    draw(cx, window);
    let names: Vec<String> = view.read_with(cx, |view, _| {
        view.enclosing()
            .into_iter()
            .map(|item| item.name.clone())
            .collect()
    });
    assert_eq!(names, ["Owner", "getOwner"]);
    assert!(
        bounds_of(cx, window, "editor-crumbs").size.height > px(0.),
        "the breadcrumbs are on screen"
    );
    assert!(bounds_of(cx, window, "crumb-1").size.height > px(0.));
}

#[ui_kit::gpui::test]
fn find_in_files_opens_a_hit_and_replace_in_files_writes_the_disk(cx: &mut TestAppContext) {
    let root = scratch("find-files");
    let first = root.join("a.txt");
    let second = root.join("b.txt");
    fs::write(&first, "alpha one\n").unwrap();
    fs::write(&second, "beta alpha\n").unwrap();
    let window = open_window(cx, root.clone());
    wait_for_files(cx, window, &first);

    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "finder").size.height > px(0.),
        "Ctrl+Shift+F opens Find in Files"
    );
    type_text(cx, window, "alpha");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(active_path(cx, window), Some(first.clone()));
    let view = editor_of(cx, window, &first);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // Ctrl+Shift+R replaces in the open and the closed file, all of it on disk.
    press(cx, window, "ctrl-shift-r");
    draw(cx, window);
    type_text(cx, window, "alpha");
    press(cx, window, "tab");
    type_text(cx, window, "omega");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the replaced files", |_| {
        fs::read_to_string(&first).is_ok_and(|text| text.contains("omega"))
            && fs::read_to_string(&second).is_ok_and(|text| text.contains("omega"))
    });
    assert_eq!(fs::read_to_string(&first).unwrap(), "omega one\n");
    assert_eq!(fs::read_to_string(&second).unwrap(), "beta omega\n");
    assert_eq!(buffer_of(cx, &view), "omega one\n");
}

fn clear(cx: &mut TestAppContext, view: &gpui::Entity<EditorView>) {
    view.update(cx, |view, cx| {
        let length = view.text().map_or(0, |text| text.len());
        view.replace_text(0..length, "", cx);
        view.restore_caret(0, 0, cx);
    });
}

#[ui_kit::gpui::test]
fn live_templates_expand_by_tab_the_list_postfix_and_surround(cx: &mut TestAppContext) {
    let root = sample_project("templates");
    let path = root.join("src").join("Code.txt");
    fs::write(&path, "\n").unwrap();
    let window = open_window(cx, root.clone());
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    // An abbreviation and Tab.
    clear(cx, &view);
    type_text(cx, window, "sout");
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "print();");
    type_text(cx, window, "x");
    assert_eq!(buffer_of(cx, &view), "print(x);");

    // A declaration template with a field at the end.
    clear(cx, &view);
    type_text(cx, window, "psvm");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.starts_with("void main() {\n"), "{text:?}");
    assert!(text.ends_with("\n}"), "{text:?}");
    type_text(cx, window, "body");
    assert!(
        buffer_of(cx, &view).contains("body\n}"),
        "{:?}",
        buffer_of(cx, &view)
    );

    // Two fields with copies: Tab walks them.
    clear(cx, &view);
    type_text(cx, window, "fori");
    press(cx, window, "tab");
    type_text(cx, window, "index");
    press(cx, window, "tab");
    type_text(cx, window, "10");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(
        text.starts_with("for (int index = 0; index < 10; index++) {"),
        "{text:?}"
    );

    // Ctrl+J lists the templates that fit the word before the caret.
    clear(cx, &view);
    type_text(cx, window, "so");
    press(cx, window, "ctrl-j");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "template-popup").size.height > px(0.),
        "Ctrl+J opens the template list"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "print();");

    // A postfix key after an expression.
    clear(cx, &view);
    type_text(cx, window, "value.var");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("= value;"), "{text:?}");

    // Ctrl+Alt+T surrounds the selected lines.
    clear(cx, &view);
    type_text(cx, window, "one\ntwo");
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-alt-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "template-popup").size.height > px(0.),
        "Ctrl+Alt+T opens the Surround With list"
    );
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(text.starts_with("if (true) {"), "{text:?}");
    assert!(text.contains("one") && text.contains("two"), "{text:?}");
}

#[ui_kit::gpui::test]
fn folding_wrap_whitespace_sticky_lines_bookmarks_zoom_and_complete_statement(
    cx: &mut TestAppContext,
) {
    let root = long_project("editor-view");
    let long = root.join("src").join("Long.txt");
    let window = open_window(cx, root.clone());
    open_path(cx, window, &long);
    let view = editor_of(cx, window, &long);
    wait_for_outline(cx, window, &long);

    // Folding takes display rows away and Unfold brings them back.
    let rows = view.read_with(cx, |view, _| view.display_rows());
    wait_for(cx, "the syntax folds", |cx| {
        press(cx, window, "ctrl--");
        let folded = view.read_with(cx, |view, _| view.display_rows());
        if folded < rows {
            true
        } else {
            press(cx, window, "ctrl-=");
            false
        }
    });
    press(cx, window, "ctrl-=");
    assert_eq!(
        view.read_with(cx, |view, _| view.display_rows()),
        rows,
        "Unfold brings every row back"
    );

    // Soft wrap and whitespace are switches of the view.
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(editor_keys::TOGGLE_SOFT_WRAP, window, cx)
        })
        .unwrap();
    assert!(view.read_with(cx, |view, _| view.soft_wrap()));
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(editor_keys::TOGGLE_WHITESPACE, window, cx)
        })
        .unwrap();
    assert!(view.read_with(cx, |view, _| view.show_whitespace()));

    // Sticky lines pin the declarations the view is inside.
    view.update(cx, |view, cx| view.restore_caret(80, 0, cx));
    draw(cx, window);
    let sticky = view.read_with(cx, |view, _| view.sticky_lines().to_vec());
    assert!(sticky.contains(&0), "the class stays on top: {sticky:?}");
    assert!(sticky.contains(&1), "the method stays on top: {sticky:?}");

    // F11 marks the line, Shift+F11 lists the marks.
    view.update(cx, |view, cx| view.restore_caret(10, 0, cx));
    draw(cx, window);
    press(cx, window, "f11");
    let marks = cx.update(|cx| editor_view::bookmarks(cx));
    assert!(
        marks
            .iter()
            .any(|entry| entry.path == long && entry.line == 10),
        "{marks:?}"
    );
    press(cx, window, "shift-f11");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "places-popup").size.height > px(0.),
        "Shift+F11 lists the bookmarks"
    );
    press(cx, window, "escape");
    press(cx, window, "f11");
    assert!(
        cx.update(|cx| editor_view::bookmarks(cx))
            .iter()
            .all(|entry| entry.path != long),
        "F11 again takes the mark away"
    );

    // Ctrl+wheel zooms the text.
    let bounds = bounds_of(cx, window, "editor-body-0");
    let center = bounds.center();
    let before = view.read_with(cx, |view, _| view.font_size());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(center, None, Modifiers::default());
    visual.simulate_event(ScrollEvent {
        position: center,
        delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        ..ScrollEvent::default()
    });
    cx.run_until_parked();
    let after = view.read_with(cx, |view, _| view.font_size());
    assert_ne!(before, after, "Ctrl+wheel changes the font size");

    // Ctrl+Shift+Enter finishes a statement on the caret line.
    let statement = root.join("src").join("Stmt.txt");
    fs::write(&statement, "doThing").unwrap();
    open_path(cx, window, &statement);
    let view = editor_of(cx, window, &statement);
    view.update(cx, |view, cx| view.restore_caret(0, 7, cx));
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(buffer_of(cx, &view), "doThing;\n");
}

#[ui_kit::gpui::test]
fn the_status_bar_shows_the_caret_and_changes_the_file_format(cx: &mut TestAppContext) {
    let root = scratch("status-ui");
    let path = root.join("Status.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 2, cx));
    draw(cx, window);

    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .expect("the window is open")
        .expect("the open tab has a status");
    assert_eq!(status.caret, "2:3");
    assert_eq!(status.eol, "LF");
    assert_eq!(status.encoding, "UTF-8");

    // The line separator of the status bar, chosen from the menu of the widget.
    click(cx, window, "st-eol");
    draw(cx, window);
    click(cx, window, "menu-1");
    draw(cx, window);
    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .expect("the window is open")
        .expect("the open tab has a status");
    assert_eq!(status.eol, "CRLF");
    assert!(
        view.update(cx, |view, cx| view.save_blocking(cx)).is_none(),
        "the file is written"
    );
    assert_eq!(fs::read(&path).unwrap(), b"alpha\r\nbeta\r\n");

    // The encoding of the status bar.
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "\u{043f}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}", cx)
    });
    click(cx, window, "st-enc");
    draw(cx, window);
    click(cx, window, "menu-3");
    draw(cx, window);
    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .expect("the window is open")
        .expect("the open tab has a status");
    assert_eq!(status.encoding, "windows-1251");
    assert!(
        view.update(cx, |view, cx| view.save_blocking(cx)).is_none(),
        "the file is written"
    );
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        bytes[0], 0xef,
        "the first letter is written in windows-1251"
    );
}

#[ui_kit::gpui::test]
fn reload_and_autosave_take_and_write_the_disk_text(cx: &mut TestAppContext) {
    let root = scratch("reload-ui");
    let path = root.join("Reload.txt");
    fs::write(&path, "first\n").unwrap();
    let window = open_window(cx, root.clone());
    // Go to File finds the file once the index has scanned, and the index watches the
    // disk before it scans: the change below is not made before anyone looks.
    wait_for_files(cx, window, &path);
    let view = editor_of(cx, window, &path);

    // A clean buffer takes the text from disk as it changes.
    fs::write(&path, "disk\n").unwrap();
    wait_for(cx, "the watcher to reload the file", |cx| {
        buffer_of(cx, &view) == "disk\n"
    });
    assert!(!view.read_with(cx, |view, _| view.is_dirty()));

    // Edits of the buffer win against the disk until Reload from Disk.
    view.update(cx, |view, cx| view.replace_text(0..4, "mine", cx));
    fs::write(&path, "other\n").unwrap();
    view.update(cx, |view, cx| view.reload(cx));
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), "mine\n", "a conflict keeps the edits");
    // The tab menu has Reload from Disk: the disk text replaces the edits.
    let bounds = bounds_of(cx, window, "tab-0-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "menu-10").size.height > px(0.),
        "the tab menu lists Reload from Disk"
    );
    click(cx, window, "menu-10");
    wait_for(cx, "Reload from Disk", |cx| {
        buffer_of(cx, &view) == "other\n"
    });

    // Leaving the window writes the unsaved text.
    view.update(cx, |view, cx| view.replace_text(0..5, "saved", cx));
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, _| window.activate_window());
    visual.deactivate_window();
    wait_for(cx, "the autosave", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "saved\n")
    });
    assert!(!view.read_with(cx, |view, _| view.is_dirty()));
}

#[ui_kit::gpui::test]
fn todo_opens_a_mark_and_the_terminal_opens_and_closes(cx: &mut TestAppContext) {
    let root = sample_project("todo-ui");
    let notes = root.join("src").join("notes.txt");
    let window = open_window(cx, root.clone());

    let todo = cx.update(|cx| cx.global::<ActiveTodo>().0.clone());
    wait_for(cx, "the TODO scan", |cx| {
        !todo.read_with(cx, |panel, _| panel.items().is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TODO_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let items = todo.read_with(cx, |panel, _| panel.items().to_vec());
    assert!(
        items.iter().any(|item| item.label == "TODO read me"),
        "{items:?}"
    );
    press(cx, window, "down enter");
    assert_eq!(active_path(cx, window), Some(notes.clone()));
    let view = editor_of(cx, window, &notes);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));

    // Alt+F12 opens the Terminal tool window and closes it again.
    press(cx, window, "alt-f12");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "Alt+F12 opens the Terminal"
    );
    draw(cx, window);
    let terminal = cx.update(|cx| cx.global::<ActiveTerminal>().0.clone());
    wait_for(cx, "the shell", |cx| {
        terminal.read_with(cx, |panel, _| panel.session().is_some())
            || !window
                .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
                .unwrap()
    });
    if let Some(session) = terminal.read_with(cx, |panel, _| panel.session().cloned()) {
        assert!(
            session.send(b"echo hello\r".to_vec()).is_ok(),
            "the shell takes a command"
        );
    } else {
        eprintln!(
            "the terminal did not start here: {}",
            terminal.read_with(cx, |panel, _| panel.report())
        );
    }
    press(cx, window, "alt-f12");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "Alt+F12 closes the Terminal"
    );
}

#[ui_kit::gpui::test]
fn empty_windows_and_a_missing_file_do_not_panic(cx: &mut TestAppContext) {
    let root = sample_project("empty-ui");
    let dockerfile = root.join("Dockerfile");
    let window = open_window(cx, root);

    // No file is open: the windows that need one show their empty state.
    for (command, keys) in [
        (keys::GOTO_FILE, "escape"),
        (keys::GOTO_CLASS, "escape"),
        (keys::GOTO_SYMBOL, "escape"),
        (keys::SEARCH_EVERYWHERE, "escape"),
        (keys::RECENT_FILES, "escape"),
        (keys::RECENT_LOCATIONS, "escape"),
        (keys::FILE_STRUCTURE, "escape"),
        (keys::FIND_IN_FILES, "escape"),
        (keys::REPLACE_IN_FILES, "escape"),
    ] {
        open_palette(cx, window, command);
        draw(cx, window);
        press(cx, window, keys);
        draw(cx, window);
    }
    for key in ["ctrl-e", "ctrl-shift-e", "ctrl-f12", "shift-f11", "alt-7"] {
        press(cx, window, key);
        draw(cx, window);
    }
    press(cx, window, "escape");
    press(cx, window, "alt-7");

    // A language without templates takes Ctrl+J and Ctrl+Alt+T without a list.
    open_path(cx, window, &dockerfile);
    press(cx, window, "ctrl-j");
    draw(cx, window);
    press(cx, window, "ctrl-alt-t");
    draw(cx, window);
    press(cx, window, "escape");
    press(cx, window, "ctrl--");
    press(cx, window, "ctrl-=");
    press(cx, window, "ctrl-shift-enter");
    draw(cx, window);

    // Reload from Disk of a file that is gone keeps the buffer.
    let view = editor_of(cx, window, &dockerfile);
    let before = buffer_of(cx, &view);
    fs::remove_file(&dockerfile).unwrap();
    view.update(cx, |view, cx| view.reload_from_disk(cx));
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), before);
}

#[ui_kit::gpui::test]
fn services_without_docker_show_an_empty_state(cx: &mut TestAppContext) {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let root = sample_project("services-ui");
    let missing = scratch("no-docker").join("docker.exe");
    // SAFETY: the test holds `ENV`, the window reads the variable at creation.
    unsafe { std::env::set_var("IDE_DOCKER_CLI", &missing) };
    let window = open_window(cx, root);
    // SAFETY: the window has read the variable already.
    unsafe { std::env::remove_var("IDE_DOCKER_CLI") };

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    let report = docker.read_with(cx, |panel, _| panel.report());
    assert!(
        report[0].starts_with("docker daemon"),
        "the panel says what it knows about the daemon: {report:?}"
    );
    assert!(
        !report
            .iter()
            .any(|line| line.starts_with("docker container ")),
        "there is no container without Docker: {report:?}"
    );
}

#[ui_kit::gpui::test]
fn a_plugin_command_reaches_find_action(cx: &mut TestAppContext) {
    let _guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    let plugins = scratch("plugins");
    let folder = plugins.join("demo");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("plugin.toml"),
        "id = \"demo\"\nname = \"Demo\"\nversion = \"1.0\"\napi = 1\ncommand = [\"no-such-plugin-process\"]\n\n[[contributes.commands]]\nid = \"ping\"\ntitle = \"Demo ping\"\n",
    )
    .unwrap();
    let root = sample_project("plugin-ui");
    // SAFETY: the test holds `ENV`, the window reads the variable at creation.
    unsafe { std::env::set_var("IDE_PLUGINS_DIR", &plugins) };
    let window = open_window(cx, root);
    // SAFETY: the window has loaded the plugins already.
    unsafe { std::env::remove_var("IDE_PLUGINS_DIR") };

    let state = cx.update(|cx| cx.global::<ActivePlugins>().0.clone());
    let report = state.read_with(cx, |plugins, _| plugins.report());
    assert!(
        report
            .iter()
            .any(|line| line == "plugin command demo.ping Demo ping"),
        "{report:?}"
    );

    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, "demo");
    draw(cx, window);
    press(cx, window, "enter");
    wait_for(cx, "the plugin command", |cx| {
        state
            .read_with(cx, |plugins, _| plugins.report())
            .iter()
            .any(|line| line.starts_with("plugin last demo.ping"))
    });
}

/// A repository with one commit on the branch `base`.
fn repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    git.run(&root, ["checkout", "-b", "base"]).unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    root
}

fn current_branch(git: &gpui::Entity<crate::git::GitState>, cx: &TestAppContext) -> Option<String> {
    let repo = git.read_with(cx, |git, _| git.repo())?;
    repo.branches()
        .ok()?
        .into_iter()
        .find(|branch| branch.current)
        .map(|branch| branch.name)
}

#[ui_kit::gpui::test]
fn the_branch_popup_creates_and_switches_a_branch(cx: &mut TestAppContext) {
    let root = repository("git-branches-ui");
    let window = open_window(cx, root.clone());
    let git = cx.update(|cx| cx.global::<ActiveGit>().0.clone());
    wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    wait_for(cx, "the branch", |cx| {
        current_branch(&git, cx).as_deref() == Some("base")
    });

    // The popup lists the branches; New Branch asks for the name.
    git.update(cx, |git, cx| {
        git.show_branches(point(px(200.), px(200.)), cx)
    });
    wait_for(cx, "the branch menu", |cx| {
        VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("menu-2")
            .is_some()
    });
    click(cx, window, "menu-0");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "prompt").size.height > px(0.),
        "New Branch asks for a name"
    );
    type_text(cx, window, "feature");
    press(cx, window, "enter");
    wait_for(cx, "the new branch", |cx| {
        current_branch(&git, cx).as_deref() == Some("feature")
    });

    // The popup switches back to the base branch.
    git.update(cx, |git, cx| {
        git.show_branches(point(px(200.), px(200.)), cx)
    });
    wait_for(cx, "the branch menu", |cx| {
        VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("menu-2")
            .is_some()
    });
    click(cx, window, "menu-2");
    wait_for(cx, "the base branch", |cx| {
        current_branch(&git, cx).as_deref() == Some("base")
    });
}

#[ui_kit::gpui::test]
fn local_history_of_a_folder_lists_its_saved_file(cx: &mut TestAppContext) {
    let root = scratch("history-folder-ui");
    let path = root.join("Saved.txt");
    fs::write(&path, "one\n").unwrap();
    let window = open_window(cx, root.clone());
    wait_for(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..3, "two", cx);
        assert!(view.save_blocking(cx).is_none(), "the file is written");
    });
    wait_for(cx, "the saved version", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| !history.folder_history(&root).is_empty())
        })
    });

    window
        .update(cx, |workspace, window, cx| {
            crate::history_view::HistoryView::show(root.clone(), workspace, window, cx)
        })
        .unwrap();
    wait_for(cx, "the history window", |cx| {
        cx.update(|cx| crate::history_view::report(cx))
            .iter()
            .any(|line| line.starts_with("history-version"))
    });
    let report = cx.update(|cx| crate::history_view::report(cx));
    assert!(
        report.iter().any(|line| line.contains("Saved.txt")),
        "the folder lists the version of its file: {report:?}"
    );
}

/// A repository with one commit and a bare clone of it, for Get from VCS.
fn bare_repository(name: &str) -> (PathBuf, PathBuf) {
    let source = repository(name);
    let bare = scratch(&format!("{name}-bare"));
    fs::remove_dir_all(&bare).unwrap();
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    let parent = bare.parent().unwrap().to_path_buf();
    git.run(
        &parent,
        [
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    )
    .unwrap();
    (bare, source)
}

#[ui_kit::gpui::test]
fn the_start_screen_opens_creates_and_clones(cx: &mut TestAppContext) {
    let root = sample_project("start-ui");
    let templates = vec![ProjectTemplate {
        label: "Plain",
        files: |name| vec![(format!("{name}.txt"), format!("{name}\n"))],
    }];
    let screen = cx.add_window(|window, cx| {
        cx.set_global(Theme::dark());
        let mut screen = StartScreen::new_for_test(
            vec![root.clone()],
            scratch("start-state-ui"),
            templates,
            window,
            cx,
        );
        screen.keep_window_for_test();
        screen
    });
    cx.run_until_parked();
    draw(cx, screen);

    // Open asks the platform for a folder.
    click(cx, screen, "start-open");
    assert!(cx.did_prompt_for_paths(), "Open asks for a folder");
    cx.simulate_path_prompt_response(|_| Some(vec![root.clone()]));
    cx.run_until_parked();

    // New Project writes the files of the template into the chosen folder.
    let parent = scratch("start-new-parent");
    let name = "made";
    let folder = parent.join(name);
    screen
        .update(cx, |screen, _, cx| {
            screen.set_fields_for_test(name, &parent.display().to_string(), "");
            cx.notify();
        })
        .unwrap();
    click(cx, screen, "start-new");
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.mode_name())
            .unwrap(),
        "new"
    );
    click(cx, screen, "start-create");
    wait_for(cx, "the new project", |_| folder.is_dir());
    assert_eq!(
        fs::read_to_string(folder.join("made.txt")).unwrap(),
        "made\n"
    );

    // Get from VCS clones a local bare repository into the chosen folder.
    let (bare, _) = bare_repository("start-clone-ui");
    let clone_parent = scratch("start-clone-parent");
    screen
        .update(cx, |screen, _, cx| {
            screen.set_fields_for_test(
                "",
                &clone_parent.display().to_string(),
                bare.to_str().unwrap(),
            );
            cx.notify();
        })
        .unwrap();
    click(cx, screen, "start-clone");
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.mode_name())
            .unwrap(),
        "clone"
    );
    let cloned = clone_parent.join(bare.file_name().unwrap());
    click(cx, screen, "start-do-clone");
    wait_for(cx, "the clone", |_| cloned.join("main.txt").is_file());
}
