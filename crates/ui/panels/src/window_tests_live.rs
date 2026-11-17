//! The fixes of the live check: Unicode packets, selection in one-line fields, the
//! completion list, double modifiers, redrawing while the list is open, the star of a
//! tab and the place a jump scrolls to. No real keyboard of Windows is involved.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::{Languages, Spot, completion_parts, expand_snippet};
use futures_util::StreamExt;
use keymap::workspace as keys;
use lang_api::{
    Answer, CompletionEntry, CompletionList, CompletionProvider, Confidence, DocumentSpot,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SourceId, SyntaxProvider,
    TextPoint, ask,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, TextField, Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::finder::Finder;
use crate::index::SearchSetup;
use crate::palette::Palette;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, line_height, open_palette, open_path, open_window,
    press, project_setup, sample_project, scratch, type_text,
};
use crate::window_tests_lang;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("live-completion");

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language whose completion list is given by the test, to see what the window does
/// with `sortText` and with snippet tab stops.
struct LiveLanguage {
    syntax: NoSyntax,
    entries: Vec<CompletionEntry>,
}

impl LiveLanguage {
    fn new(entries: Vec<CompletionEntry>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            entries,
        })
    }
}

impl LanguageSupport for LiveLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("live")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension().is_some_and(|extension| extension == "txt")
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }
}

impl Provider<DocumentSpot, CompletionList> for LiveLanguage {
    fn start(&self, _: DocumentSpot, _: Cancel, mut reply: lang_api::Reply<CompletionList>) {
        let _ = reply.send(Answer::new(
            CompletionList {
                entries: self.entries.clone(),
                complete: true,
            },
            Confidence::Certain,
            SOURCE,
        ));
    }
}

fn open_live_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<LiveLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("live-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
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
    let setup = project_setup(scratch("live-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn palette_of(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Entity<Palette>> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()?;
    view.downcast::<Palette>().ok()
}

fn palette_field(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<TextField> {
    palette_of(cx, window).map(|view| view.read_with(cx, |view, _| view.query_field().clone()))
}

fn finder_of(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<gpui::Entity<Finder>> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()?;
    view.downcast::<Finder>().ok()
}

fn finder_field(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    replace: bool,
) -> Option<TextField> {
    finder_of(cx, window).map(|view| {
        view.read_with(cx, |view, _| {
            if replace {
                view.replace_field().clone()
            } else {
                view.find_field().clone()
            }
        })
    })
}

fn double_modifier(cx: &mut TestAppContext, window: WindowHandle<Workspace>, held: Modifiers) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let change = |modifiers| ui_kit::ModifiersChangedEvent {
        modifiers,
        ..ui_kit::ModifiersChangedEvent::default()
    };
    visual.simulate_event(change(held));
    visual.simulate_event(change(Modifiers::default()));
    visual.simulate_event(change(held));
    visual.simulate_event(change(Modifiers::default()));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn ctrl_a_shift_keys_and_the_clipboard_edit_the_palette_query(cx: &mut TestAppContext) {
    let root = sample_project("live-palette-field");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_palette(cx, window, keys::GOTO_FILE);

    type_text(cx, window, "abc");
    draw(cx, window);
    assert_eq!(palette_field(cx, window).unwrap().text, "abc");

    press(cx, window, "ctrl-a");
    let field = palette_field(cx, window).unwrap();
    assert_eq!(field.selected_text(), "abc", "Ctrl+A selects the query");
    type_text(cx, window, "q");
    assert_eq!(palette_field(cx, window).unwrap().text, "q");

    press(cx, window, "ctrl-a");
    type_text(cx, window, "hello world");
    press(cx, window, "home");
    press(cx, window, "shift-right");
    assert_eq!(palette_field(cx, window).unwrap().selected_text(), "h");
    press(cx, window, "ctrl-shift-right");
    assert_eq!(
        palette_field(cx, window).unwrap().selected_text(),
        "hello",
        "Ctrl+Shift+Right selects the word"
    );
    type_text(cx, window, "X");
    assert_eq!(palette_field(cx, window).unwrap().text, "X world");

    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-c");
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("X world"));
    press(cx, window, "ctrl-x");
    assert_eq!(palette_field(cx, window).unwrap().text, "");
    press(cx, window, "ctrl-v");
    assert_eq!(palette_field(cx, window).unwrap().text, "X world");
}

#[ui_kit::gpui::test]
fn go_to_class_keeps_the_typed_query(cx: &mut TestAppContext) {
    let root = crate::window_tests_ui::navigation_project("live-goto-class");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_palette(cx, window, keys::GOTO_CLASS);
    type_text(cx, window, "Pet");
    draw(cx, window);
    assert_eq!(palette_field(cx, window).unwrap().text, "Pet");
}

#[ui_kit::gpui::test]
fn selection_and_paste_work_in_the_find_in_files_fields(cx: &mut TestAppContext) {
    let root = sample_project("live-finder-fields");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_palette(cx, window, keys::REPLACE_IN_FILES);

    type_text(cx, window, "one two");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "x");
    assert_eq!(finder_field(cx, window, false).unwrap().text, "x");

    press(cx, window, "tab");
    type_text(cx, window, "replacement");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "y");
    assert_eq!(finder_field(cx, window, true).unwrap().text, "y");
    press(cx, window, "shift-left");
    assert_eq!(finder_field(cx, window, true).unwrap().selected_text(), "y");
    press(cx, window, "backspace");
    assert_eq!(finder_field(cx, window, true).unwrap().text, "");
}

#[ui_kit::gpui::test]
fn a_method_snippet_puts_the_caret_inside_the_brackets() {
    let (text, caret) = expand_snippet("getLastName(${1:name})$0");
    assert_eq!(text, "getLastName(name)");
    assert_eq!(caret, Some("getLastName(".len()));

    let (text, caret) = expand_snippet("getLastName()");
    assert_eq!(text, "getLastName()");
    assert_eq!(
        caret, None,
        "a method without parameters ends after the brackets"
    );

    // A server without argument names gives the final tab stop inside the brackets.
    assert_eq!(expand_snippet("greet(${0})"), ("greet()".into(), Some(6)));
    assert_eq!(expand_snippet("greet($0)"), ("greet()".into(), Some(6)));
    assert_eq!(expand_snippet("size()$0"), ("size()".into(), None));
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn real_server_returns_methods_with_brackets_and_a_tab_stop(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("live-real-snippet");
    let path = window_language::write_completion_sample(&root);
    let source = r#"package probe;

public class Complete {
    static class Person {
        String last;
        String getLastName() { return last; }
        String greet(String who, int times) { return who + times; }
    }

    void show(Person person) {
        person.
    }
}
"#;
    fs::write(&path, source).unwrap();
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("live-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    language.open_document(&path).unwrap();

    let offset = source.find("person.").unwrap() + "person.".len();
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let character = before.rsplit('\n').next().unwrap().chars().count() as u32;
    let uri = platform::CanonPath::new(&path).unwrap().to_uri();
    let support = language.support();
    let provider = support.completion().unwrap();
    let mut replies = ask(
        provider,
        DocumentSpot {
            uri,
            version: 1,
            point: TextPoint { line, character },
        },
    );
    let entries: Arc<Mutex<Option<Vec<CompletionEntry>>>> = Arc::new(Mutex::new(None));
    let slot = entries.clone();
    cx.spawn(async move |_| {
        if let Some(answer) = replies.next().await {
            *slot.lock().unwrap() = Some(answer.value.entries);
        }
    })
    .detach();
    crate::window_tests_real::wait_for(
        cx,
        "the completion of the members",
        Duration::from_secs(120),
        |_| entries.lock().unwrap().is_some(),
    );
    let entries = entries.lock().unwrap().clone().unwrap();
    let method = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.label.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} in {:?}", entries.len()))
    };

    let no_parameters = method("getLastName");
    assert!(no_parameters.snippet, "the server answers with a snippet");
    let insert = no_parameters.insert_text.as_deref().unwrap();
    let (text, caret) = expand_snippet(insert);
    assert_eq!(text, "getLastName()", "the brackets come with the method");
    assert_eq!(caret, None, "the caret stays after the brackets");

    let parameters = method("greet");
    assert!(parameters.snippet);
    let insert = parameters.insert_text.as_deref().unwrap();
    let (text, caret) = expand_snippet(insert);
    // As in IDEA: no argument names, the caret between the brackets.
    assert_eq!(text, "greet()", "{insert}");
    assert_eq!(
        caret,
        Some("greet(".len()),
        "the caret is inside the brackets"
    );

    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
fn the_completion_list_keeps_sort_text_and_a_snippet_puts_the_caret_inside(
    cx: &mut TestAppContext,
) {
    let root = scratch("live-snippet-window");
    let path = root.join("Main.txt");
    fs::write(&path, "\n").unwrap();
    let entry = |label: &str, insert: &str, sort: &str, snippet: bool| CompletionEntry {
        label: label.to_owned(),
        detail: Some(format!("Person.{label}")),
        insert_text: Some(insert.to_owned()),
        sort_text: Some(sort.to_owned()),
        snippet,
        ..CompletionEntry::default()
    };
    let language = LiveLanguage::new(vec![
        entry("alpha() : String", "alpha()", "2", false),
        entry("zeta() : String", "zeta()", "0", false),
        entry(
            "getLastName() : String",
            "getLastName(${1:name})$0",
            "1",
            true,
        ),
    ]);
    let window = open_live_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    // `sortText` says zeta first, although alpha is the first label alphabetically.
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "zeta()\n");

    press(cx, window, "ctrl-z");
    draw(cx, window);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "getLastName(name)\n");
    let status = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap()
        .unwrap();
    assert_eq!(
        status.caret, "1:17 (4 chars)",
        "the caret is in the snippet's field, which is selected"
    );
}

#[ui_kit::gpui::test]
fn a_completion_row_splits_the_signature_from_the_type_and_the_owner() {
    let (left, right, owner) = completion_parts(
        "getLastName(String) : String",
        Some("Person.getLastName(String) : String"),
    );
    assert_eq!(left, "getLastName(String)");
    assert_eq!(right.as_deref(), Some("String"));
    assert_eq!(owner.as_deref(), Some("Person"));

    let (left, right, owner) = completion_parts("size", Some("org.example.List"));
    assert_eq!(left, "size");
    assert_eq!(right, None);
    assert_eq!(owner.as_deref(), Some("org.example.List"));
}

#[ui_kit::gpui::test]
fn the_completion_list_flips_above_a_caret_at_the_bottom(cx: &mut TestAppContext) {
    let source: String = (0..60).map(|line| format!("line {line}\n")).collect();
    let root = window_tests_lang::project(
        "live-completion-place",
        &[("src/Long.txt", source.as_str())],
    );
    let language = window_tests_lang::FakeLanguage::new();
    let window = window_tests_lang::open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("Long.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: 59,
                column: 0,
            },
            cx,
        )
    });
    draw(cx, window);
    type_text(cx, window, "g");
    draw(cx, window);

    let popup = bounds_of(cx, window, "completion-popup");
    let editor = bounds_of(cx, window, "editor-body-0");
    assert!(
        popup.bottom() <= editor.bottom(),
        "the list stays inside the editor: {popup:?} in {editor:?}"
    );
    assert!(
        popup.top() >= editor.top(),
        "the list does not leave the top of the editor: {popup:?} in {editor:?}"
    );
}

#[ui_kit::gpui::test]
fn typing_and_the_status_caret_move_while_the_completion_list_is_open(cx: &mut TestAppContext) {
    let root = window_tests_lang::project("live-completion-redraw", &[("src/Main.txt", "\n")]);
    let language = window_tests_lang::FakeLanguage::new();
    let window = window_tests_lang::open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("Main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "g");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "the list of the server is open"
    );
    let before = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap();
    assert_eq!(before.unwrap().caret, "1:2");

    type_text(cx, window, "r");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "gr\n",
        "the typed letter is in the text"
    );
    let after = window
        .read_with(cx, |workspace, cx| workspace.editor_status(cx))
        .unwrap();
    assert_eq!(
        after.unwrap().caret,
        "1:3",
        "the status caret follows the key"
    );
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "the list stays open while typing"
    );
}

#[ui_kit::gpui::test]
fn ctrl_z_does_not_count_as_a_double_ctrl(cx: &mut TestAppContext) {
    let root = sample_project("live-double-ctrl");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    draw(cx, window);

    press(cx, window, "ctrl-z");
    press(cx, window, "ctrl-z");
    draw(cx, window);
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "Ctrl+Z twice does not open Run Anything"
    );

    double_modifier(
        cx,
        window,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "Ctrl alone twice still opens Run Anything"
    );
}

#[ui_kit::gpui::test]
fn undo_to_the_saved_text_clears_the_dirty_mark(cx: &mut TestAppContext) {
    let root = scratch("live-dirty-star");
    let path = root.join("Main.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "X");
    draw(cx, window);
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    assert!(
        window
            .read_with(cx, |workspace, cx| workspace.editor_status(cx))
            .unwrap()
            .unwrap()
            .dirty
    );

    press(cx, window, "ctrl-z");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");
    assert!(
        !view.read_with(cx, |view, _| view.is_dirty()),
        "the text is the saved text again"
    );
    assert!(
        !window
            .read_with(cx, |workspace, cx| workspace.editor_status(cx))
            .unwrap()
            .unwrap()
            .dirty,
        "the tab no longer shows the star"
    );
}

#[ui_kit::gpui::test]
fn a_jump_to_a_place_scrolls_it_near_the_middle(cx: &mut TestAppContext) {
    let root = scratch("live-reveal-center");
    let path = root.join("Long.txt");
    let text: String = (0..120).map(|line| format!("line {line}\n")).collect();
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let line = 90;

    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line, column: 0 }, cx)
    });
    draw(cx, window);

    let height = f32::from(bounds_of(cx, window, "editor-body-0").size.height);
    let line_height = line_height(cx);
    let scroll = view.read_with(cx, |view, _| view.scroll_top());
    let expected = line as f32 * line_height - (height - line_height) / 2.;
    assert!(
        (scroll - expected).abs() <= line_height * 2.,
        "the line is near the middle: scroll {scroll}, expected about {expected}"
    );
}

#[ui_kit::gpui::test]
fn copy_and_paste_in_the_project_tree_do_not_count_as_a_double_ctrl(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "live-tree-double-ctrl");
    let panel = cx.update(|cx| cx.global::<crate::project::ActiveProjectPanel>().0.clone());
    let file = root.join("src").join("main.txt");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::PROJECT_TOOL, window, cx)
        })
        .unwrap();
    cx.update(|cx| panel.update(cx, |panel, cx| panel.select_path(&file, cx)));
    cx.run_until_parked();
    draw(cx, window);

    // The tree takes Ctrl+C and Ctrl+V as actions, so only the interceptor of the
    // window sees them between the presses of Ctrl.
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    let change = |modifiers| ui_kit::ModifiersChangedEvent {
        modifiers,
        ..ui_kit::ModifiersChangedEvent::default()
    };
    for keys in ["ctrl-c", "ctrl-v"] {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_event(change(ctrl));
        cx.simulate_keystrokes(window.into(), keys);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_event(change(Modifiers::default()));
        cx.run_until_parked();
    }
    draw(cx, window);
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "Ctrl+C and Ctrl+V in the tree do not open Run Anything"
    );
}
