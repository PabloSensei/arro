//! Message bundles: Ctrl+B from a source string and a Thymeleaf attribute to the key of
//! a `.properties` file, the keys in completion, the checks of both sides, Create
//! property, Find Usages and a rename. The bundles are files of the test project, so no
//! server and no outside project is needed.
//!
//! The real PetClinic check needs `IDE_CORPUS`:
//!
//! `cargo test -p panels window_tests_resource_bundles -- --ignored --test-threads=1`

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages};
use lang_api::LanguageSupport;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    active_path, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_real::wait_for;
use crate::window_tests_usages_tabs::{counts, hub};
use crate::{AgentOptions, LanguageServer, connect, install};

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("bundles-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: file_syntax::language_for,
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
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(support),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("bundles-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// A project of the test: the files are written before the support reads them.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
    }
    root
}

fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count();
    let start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    (line, text[start..offset].chars().count())
}

/// Opens a file and puts the caret on the first occurrence of `needle`, `after` bytes
/// into it.
fn open_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    needle: &str,
    after: usize,
) -> (gpui::Entity<EditorView>, usize) {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    let text = buffer_of(cx, &view);
    let offset = text.find(needle).unwrap_or(0);
    let (line, column) = line_and_column(&text, offset + after);
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();
    (view, offset)
}

fn marks(
    view: &gpui::Entity<EditorView>,
    cx: &TestAppContext,
) -> Vec<(lang_api::Severity, std::ops::Range<usize>)> {
    view.read_with(cx, |view, _| view.diagnostic_marks())
}

fn popup_shown(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

const BASE: &str = "src/main/resources/messages.properties";
const RU: &str = "src/main/resources/messages_ru.properties";

fn source_with(literal: &str) -> String {
    format!(
        "package com.example;\n\nclass App {{\n    String text(MessageSource source) {{\n        return source.getMessage(\"{literal}\", null, null);\n    }}\n}}\n"
    )
}

#[ui_kit::gpui::test]
fn ctrl_b_of_a_source_key_opens_its_line_in_the_bundle(cx: &mut TestAppContext) {
    let root = project(
        "bundles-source-nav",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Привет\n")],
    );
    let source = window_language::write_test_source(&root, "App", &source_with("welcome"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_at(cx, window, &source, "welcome", 0);

    press(cx, window, "ctrl-b");
    wait_for(cx, "the key of the bundle", Duration::from_secs(10), |cx| {
        active_path(cx, window).as_deref() == Some(base.as_path())
    });
    let view = editor_of(cx, window, &base);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 0),
        "the caret is on the key of the base file"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_of_a_thymeleaf_key_opens_its_line_in_the_bundle(cx: &mut TestAppContext) {
    let html = "src/main/resources/templates/welcome.html";
    let root = project(
        "bundles-template-nav",
        &[
            (BASE, "welcome=Welcome\n"),
            (html, "<p th:text=\"#{welcome}\">x</p>\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_at(cx, window, &root.join(html), "welcome", 0);

    press(cx, window, "ctrl-b");
    wait_for(cx, "the key of the bundle", Duration::from_secs(10), |cx| {
        active_path(cx, window).as_deref() == Some(base.as_path())
    });
}

#[ui_kit::gpui::test]
fn the_keys_of_the_bundle_complete_in_a_source_and_in_a_template(cx: &mut TestAppContext) {
    let html = "src/main/resources/templates/welcome.html";
    let root = project(
        "bundles-completion",
        &[
            (BASE, "welcome=Welcome\nwelcome.back=Back\nfarewell=Bye\n"),
            (html, "<p th:text=\"#{wel}\">x</p>\n"),
        ],
    );
    let source = window_language::write_test_source(&root, "App", &source_with("wel"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);

    let (view, _) = open_at(cx, window, &source, "wel", 3);
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the keys of the bundle",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window, "completion-popup")
        },
    );
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(
        text.contains("\"welcome\""),
        "the key is written from its prefix: {text}"
    );

    let (view, _) = open_at(cx, window, &root.join(html), "wel", 3);
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the keys in the template",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window, "completion-popup")
        },
    );
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(
        text.contains("#{welcome}"),
        "the key is written into the attribute: {text}"
    );
}

#[ui_kit::gpui::test]
fn an_unknown_key_warns_and_create_property_adds_the_line(cx: &mut TestAppContext) {
    let root = project("bundles-create-property", &[(BASE, "welcome=Welcome\n")]);
    let source = window_language::write_test_source(&root, "App", &source_with("missing"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (view, _) = open_at(cx, window, &source, "missing", 0);

    wait_for(
        cx,
        "the warning of the unknown key",
        Duration::from_secs(10),
        |cx| {
            marks(&view, cx)
                .iter()
                .any(|(severity, _)| *severity == lang_api::Severity::Warning)
        },
    );
    press(cx, window, "alt-enter");
    wait_for(cx, "the quick fixes", Duration::from_secs(10), |cx| {
        draw(cx, window);
        popup_shown(cx, window, "actions-popup")
    });
    press(cx, window, "enter");

    let base = root.join(BASE);
    wait_for(
        cx,
        "the new line of the base file",
        Duration::from_secs(10),
        |_| {
            fs::read_to_string(&base)
                .map(|text| text.contains("missing="))
                .unwrap_or(false)
        },
    );
    assert_eq!(
        fs::read_to_string(&base).unwrap(),
        "welcome=Welcome\nmissing=\n",
        "the property is added to the base file"
    );
}

#[ui_kit::gpui::test]
fn a_properties_file_reports_a_missing_translation_and_an_unused_key(cx: &mut TestAppContext) {
    let base_text = "welcome=Welcome\norphan=Never\n";
    let root = project(
        "bundles-properties-checks",
        &[(BASE, base_text), (RU, "orphan=Никогда\n")],
    );
    window_language::write_test_source(&root, "App", &source_with("welcome"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = {
        let path = root.join(BASE);
        open_path(cx, window, &path);
        editor_of(cx, window, &path)
    };

    wait_for(
        cx,
        "the checks of the properties file",
        Duration::from_secs(10),
        |cx| marks(&view, cx).len() == 2,
    );
    let marks = marks(&view, cx);
    let line_of = |offset: usize| -> usize {
        base_text[..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    };
    let welcome = line_of(base_text.find("welcome").unwrap());
    let orphan = line_of(base_text.find("orphan").unwrap());
    assert!(
        marks
            .iter()
            .any(|(severity, range)| *severity == lang_api::Severity::Warning
                && base_text[..range.start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    == welcome),
        "the key without a translation warns: {marks:?}"
    );
    assert!(
        marks
            .iter()
            .any(|(severity, range)| *severity == lang_api::Severity::Hint
                && base_text[..range.start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    == orphan),
        "the unused key is grey: {marks:?}"
    );
}

#[ui_kit::gpui::test]
fn find_usages_of_a_key_lists_the_source_and_template_places(cx: &mut TestAppContext) {
    let html = "src/main/resources/templates/welcome.html";
    let root = project(
        "bundles-find-usages",
        &[
            (BASE, "welcome=Welcome\n"),
            (html, "<p th:text=\"#{welcome}\">x</p>\n"),
        ],
    );
    let source = window_language::write_test_source(&root, "App", &source_with("welcome"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_at(cx, window, &root.join(BASE), "welcome", 0);

    press(cx, window, "alt-f7");
    wait_for(cx, "the places of the key", Duration::from_secs(10), |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && counts(cx) == 2
    });
    let items = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    let paths: BTreeSet<PathBuf> = items.into_iter().map(|item| item.path).collect();
    assert!(paths.contains(&source), "{paths:?}");
    assert!(paths.contains(&root.join(html)), "{paths:?}");
}

#[ui_kit::gpui::test]
fn a_rename_of_a_key_changes_every_file_and_place(cx: &mut TestAppContext) {
    let html = "src/main/resources/templates/welcome.html";
    let root = project(
        "bundles-rename",
        &[
            (BASE, "welcome=Welcome\n"),
            (RU, "welcome=Привет\n"),
            (html, "<p th:text=\"#{welcome}\">x</p>\n"),
        ],
    );
    let source = window_language::write_test_source(&root, "App", &source_with("welcome"));
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (view, _) = open_at(cx, window, &root.join(BASE), "welcome", 0);

    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        popup_shown(cx, window, "rename-popup"),
        "Shift+F6 asks for the new name"
    );
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "hello");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "welcome=Welcome\n",
        "the first Enter only previews"
    );
    // The answer of the preview comes from the background worker, so the next Enter
    // may ask again until it is ready.
    wait_for(cx, "the renamed key", Duration::from_secs(10), |cx| {
        press(cx, window, "enter");
        draw(cx, window);
        buffer_of(cx, &view).starts_with("hello=")
    });
    cx.run_until_parked();
    draw(cx, window);

    wait_for(cx, "the renamed key", Duration::from_secs(10), |cx| {
        let base = buffer_of(cx, &view);
        let ru = fs::read_to_string(root.join(RU)).unwrap_or_default();
        let source = fs::read_to_string(&source).unwrap_or_default();
        let html = fs::read_to_string(root.join(html)).unwrap_or_default();
        base.starts_with("hello=")
            && ru.starts_with("hello=")
            && source.contains("getMessage(\"hello\"")
            && html.contains("#{hello}")
    });
}

/// Every key of a bundle directory and the locales that do not have it.
fn keys_without_a_locale(root: &Path) -> (usize, HashMap<String, Vec<String>>) {
    let folder = root.join("src/main/resources/messages");
    let mut files: Vec<(Option<String>, BTreeSet<String>)> = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(&folder)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for path in entries {
        if path
            .extension()
            .is_none_or(|extension| extension != "properties")
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let locale = stem
            .strip_prefix("messages_")
            .map(str::to_owned)
            .filter(|locale| !locale.is_empty());
        let keys: BTreeSet<String> = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                    return None;
                }
                let end = line.find(['=', ':', ' ']).unwrap_or(line.len());
                let key = line[..end].trim();
                (!key.is_empty()).then(|| key.to_owned())
            })
            .collect();
        files.push((locale, keys));
    }
    let mut all = BTreeSet::new();
    for (_, keys) in &files {
        all.extend(keys.iter().cloned());
    }
    let mut missing: HashMap<String, Vec<String>> = HashMap::new();
    for key in &all {
        let locales: Vec<String> = files
            .iter()
            // A locale file without a single key inherits the base file.
            .filter(|(locale, keys)| locale.is_some() && !keys.is_empty() && !keys.contains(key))
            .filter_map(|(locale, _)| locale.clone())
            .collect();
        if !locales.is_empty() {
            missing.insert(key.clone(), locales);
        }
    }
    (all.len(), missing)
}

#[tokio::test]
#[ignore = "needs IDE_CORPUS with PetClinic"]
async fn petclinic_welcome_goes_to_its_key_and_the_missing_translations_are_reported() {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("bundles-petclinic").join("petclinic");
    window_language::prepare_project(&source, &root);
    let template = root.join("src/main/resources/templates/welcome.html");
    let bundle = root.join("src/main/resources/messages/messages.properties");
    let text = fs::read_to_string(&template).expect("PetClinic has welcome.html");
    let offset = text.find("#{welcome}").expect("welcome.html uses welcome");
    let (line, column) = line_and_column(&text, offset + 2);

    let bundles = window_language::message_bundles(&root);
    let key_text = fs::read_to_string(&bundle).unwrap_or_default();
    let key_line = key_text
        .lines()
        .position(|line| line.trim_start().starts_with("welcome="));
    let spot = lang_api::DocumentSpot {
        uri: platform::CanonPath::new(&template).unwrap().to_uri(),
        version: 1,
        point: lang_api::TextPoint {
            line: line as u32,
            character: column as u32,
        },
    };
    let navigation = bundles.navigation().expect("the bundles navigate");
    let mut replies = lang_api::ask(
        navigation,
        lang_api::NavigationQuery {
            kind: lang_api::NavigationKind::Definition,
            spot: lang_api::DocumentSpot { ..spot },
        },
    );
    use futures_util::StreamExt;
    let answer = replies.next().await.expect("the key has a place");
    let target = answer.value.first().expect("welcome has a base file");
    let expected = platform::CanonPath::new(&bundle).unwrap().to_uri();
    assert_eq!(target.uri, expected, "the key leads to messages.properties");
    assert_eq!(
        Some(target.span.start.line as usize),
        key_line,
        "the caret line of welcome= is the target"
    );

    let (total, missing) = keys_without_a_locale(&root);
    eprintln!(
        "petclinic: {total} keys, {} keys without a translation in at least one locale: {:?}",
        missing.len(),
        missing
    );
    assert!(total > 0, "the bundle of PetClinic has keys");
}
