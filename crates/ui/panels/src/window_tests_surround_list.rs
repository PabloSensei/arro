//! The Surround With list of the editor: the order and the keys of the IDEA templates,
//! the selection a key wraps, and Ctrl+J inside a word that matches no abbreviation.
//! The language is the test one of `window_tests_ui`: no server and no real sources.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use editor_view::{
    EditorView, Languages, Spot, TEMPLATE_ANY, Template, TemplateKind, TemplateLanguage,
};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const CONTEXTS: &[&str] = &[TEMPLATE_ANY];

const fn surround(key: &'static str, description: &'static str, body: &'static str) -> Template {
    Template {
        key,
        description,
        kind: TemplateKind::Surround,
        body,
        fields: &[],
        contexts: CONTEXTS,
    }
}

const fn surround_live(
    key: &'static str,
    description: &'static str,
    body: &'static str,
) -> Template {
    Template {
        key,
        description,
        kind: TemplateKind::SurroundLive,
        body,
        fields: &[],
        contexts: CONTEXTS,
    }
}

const fn abbreviation(
    key: &'static str,
    description: &'static str,
    body: &'static str,
) -> Template {
    Template {
        key,
        description,
        kind: TemplateKind::Abbreviation,
        body,
        fields: &[],
        contexts: CONTEXTS,
    }
}

/// The standard Surround With entries in the order of IDEA, then the live templates, and
/// one abbreviation for Ctrl+J.
static TEMPLATES: &[Template] = &[
    surround("if", "if", "if ($CONDITION$) {\n\t$SELECTION$\n}$END$"),
    surround(
        "ifelse",
        "if / else",
        "if ($CONDITION$) {\n\t$SELECTION$\n} else {\n\t$END$\n}",
    ),
    surround(
        "while",
        "while",
        "while ($CONDITION$) {\n\t$SELECTION$\n}$END$",
    ),
    surround(
        "dowhile",
        "do / while",
        "do {\n\t$SELECTION$\n} while ($CONDITION$);$END$",
    ),
    surround(
        "for",
        "for",
        "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$SELECTION$\n}$END$",
    ),
    surround(
        "try",
        "try / catch",
        "try {\n\t$SELECTION$\n} catch (Exception e) {\n\t$END$\n}",
    ),
    surround(
        "tryfinally",
        "try / finally",
        "try {\n\t$SELECTION$\n} finally {\n\t$END$\n}",
    ),
    surround(
        "trycatchfinally",
        "try / catch / finally",
        "try {\n\t$SELECTION$\n} catch (Exception e) {\n\t$END$\n} finally {\n\t\n}",
    ),
    surround(
        "synchronized",
        "synchronized",
        "synchronized (this) {\n\t$SELECTION$\n}$END$",
    ),
    surround(
        "runnable",
        "Runnable",
        "new Runnable() {\n\t@Override\n\tpublic void run() {\n\t\t$SELECTION$\n\t}\n};$END$",
    ),
    surround("block", "{ }", "{\n\t$SELECTION$\n}$END$"),
    surround_live(
        "C",
        "Callable",
        "new Callable<Object>() {\n\t@Override\n\tpublic Object call() throws Exception {\n\t\t$SELECTION$\n\t\treturn $END$;\n\t}\n};",
    ),
    surround_live(
        "RL",
        "readLock / unlock",
        "lock.readLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\tlock.readLock().unlock();\n}$END$",
    ),
    surround_live(
        "WL",
        "writeLock / unlock",
        "lock.writeLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\tlock.writeLock().unlock();\n}$END$",
    ),
    surround_live(
        "I",
        "iterate",
        "for (Object item : items) {\n\t$SELECTION$\n}$END$",
    ),
    abbreviation("println", "Prints a line", "print($END$);"),
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
        postfix: None,
        query: OnceLock::new(),
        declarations: None,
    })
}

/// The language of a `.txt` file of the tests: the Dockerfile grammar for a tree and the
/// templates of the test.
fn resolve(path: &Path) -> Option<editor_view::LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.indent_unit = "    ";
    Some(config)
}

fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    let extension = path.extension()?.to_str()?;
    extension.eq_ignore_ascii_case("txt").then(test_language)
}

/// The window of these tests: the same order as `panels::run`, with the test language and
/// no language server.
fn open_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("surround-state");
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
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn open_sample(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (WindowHandle<Workspace>, Entity<EditorView>) {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    let path = root.join("src").join("Sample.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (window, view)
}

#[gpui::test]
fn ctrl_alt_t_lists_the_idea_surrounds_in_order_and_six_wraps_try_catch(cx: &mut TestAppContext) {
    let (window, view) = open_sample(cx, "surround-order", "one\ntwo\n");
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 0, column: 0 }, cx)
    });
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-alt-t");
    draw(cx, window);

    // The standard entries of IDEA, top to bottom: 1 if, 2 if / else, 3 while,
    // 4 do / while, 5 for, 6 try / catch, 7 try / finally, 8 try / catch / finally,
    // 9 synchronized, 0 Runnable, A { }.
    let idea = [
        "template-item-if",
        "template-item-ifelse",
        "template-item-while",
        "template-item-dowhile",
        "template-item-for",
        "template-item-try",
        "template-item-tryfinally",
        "template-item-trycatchfinally",
        "template-item-synchronized",
        "template-item-runnable",
        "template-item-block",
    ];
    let mut previous = None;
    for selector in idea {
        let top = bounds_of(cx, window, selector).origin.y;
        if let Some(previous) = previous {
            assert!(top > previous, "{selector} comes after the entry above it");
        }
        previous = Some(top);
    }
    let templates = bounds_of(cx, window, "template-item-C").origin.y;
    assert!(
        templates > previous.unwrap(),
        "the live templates follow the standard entries"
    );

    // 6 is try / catch: the digit picks it without Enter.
    press(cx, window, "6");
    let text = buffer_of(cx, &view);
    assert!(text.starts_with("try {"), "{text:?}");
    assert!(text.contains("} catch (Exception e) {"), "{text:?}");
    assert!(text.contains("one") && text.contains("two"), "{text:?}");
}

#[gpui::test]
fn if_else_wraps_the_selection_and_puts_the_caret_in_the_condition(cx: &mut TestAppContext) {
    let (window, view) = open_sample(cx, "surround-if-else", "one\ntwo\n");
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 0, column: 0 }, cx)
    });
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-alt-t");
    press(cx, window, "2");
    type_text(cx, window, "ready");

    let text = buffer_of(cx, &view);
    assert!(text.starts_with("if (ready) {"), "{text:?}");
    assert!(text.contains("} else {"), "{text:?}");
    assert!(text.contains("one") && text.contains("two"), "{text:?}");
}

#[gpui::test]
fn ctrl_j_inside_a_word_shows_every_template_and_keeps_the_word(cx: &mut TestAppContext) {
    let (window, view) = open_sample(cx, "surround-ctrl-j", "owners\n");
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 0, column: 2 }, cx)
    });
    press(cx, window, "ctrl-j");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "template-popup").size.height > px(0.),
        "Ctrl+J inside a word that matches no abbreviation shows the templates"
    );

    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert_eq!(text, "owprint();ners\n", "the word around the caret stays");
}
