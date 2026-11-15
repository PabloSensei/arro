//! Settings → Editor → Inspections and the suppression of a check from Alt+Enter,
//! against a language that answers from memory.

use std::fs;

use lang_api::{
    DiagnosticEntry, InspectionInfo, InspectionLevel, InspectionSettings, Severity, TextPoint,
    TextSpan,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use workspace::{INSPECTION_TOOL, Workspace};

use crate::inspect::ActiveInspect;
use crate::settings::SettingsView;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, line_height, open_path, press,
    project_setup, scratch, type_text, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

fn check(id: &str, title: &str) -> InspectionInfo {
    InspectionInfo {
        id: id.to_owned(),
        title: title.to_owned(),
        group: "Test".to_owned(),
        default_severity: InspectionLevel::Warning,
        configurable: true,
    }
}

fn warning(line: u32, id: &str) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint { line, character: 0 },
            end: TextPoint { line, character: 6 },
        },
        severity: Severity::Warning,
        message: "The value is never used".to_owned(),
        unnecessary: false,
        code: Some(id.to_owned()),
    }
}

fn open_inspections(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &str) {
    window
        .update(cx, |workspace, window, cx| {
            SettingsView::show_inspection(workspace, window, cx, id);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn marks(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &std::path::Path,
) -> Vec<(Severity, std::ops::Range<usize>)> {
    let view = editor_of(cx, window, path);
    view.read_with(cx, |view, _| view.diagnostic_marks())
}

fn click_editor(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(67. + column as f32 * 8.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(x, y), Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn the_page_searches_turns_a_check_off_changes_its_level_and_resets(cx: &mut TestAppContext) {
    let root = project("inspection-settings-page", &[("src/Main.txt", "value\n")]);
    let language = FakeLanguage::new();
    language.set_inspections(vec![check("native.check", "Native check")]);
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();

    open_inspections(cx, window, "native.check");
    assert!(
        has(cx, window, "settings-inspection-search"),
        "the page has its search"
    );
    assert!(
        has(cx, window, "settings-inspection-reset"),
        "the reset is drawn"
    );
    assert!(has(cx, window, "settings-inspection-0"), "the row is drawn");
    click(cx, window, "settings-inspection-search");
    type_text(cx, window, "zzz");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-inspection-search"),
        "the page stays while its search is typed"
    );
    assert!(
        !has(cx, window, "settings-inspection-0"),
        "a query with no matches hides the row"
    );
    for _ in 0..3 {
        press(cx, window, "backspace");
    }
    draw(cx, window);
    assert!(
        has(cx, window, "settings-inspection-0"),
        "the empty query shows the check"
    );

    click(cx, window, "settings-inspection-level-0");
    draw(cx, window);
    click(cx, window, "settings-ok");
    cx.run_until_parked();
    wait_for_signal(cx, "the level of the check", |_| {
        language
            .inspection_settings()
            .setting("native.check")
            .is_some_and(|setting| setting.level == Some(InspectionLevel::WeakWarning))
    });
    assert!(
        fs::read_to_string(workspace::inspections_file(&root))
            .is_ok_and(|text| text.contains("weak-warning")),
        "the level is saved in the project"
    );

    open_inspections(cx, window, "native.check");
    click(cx, window, "settings-inspection-toggle-0");
    draw(cx, window);
    click(cx, window, "settings-ok");
    cx.run_until_parked();
    wait_for_signal(cx, "the switch of the check", |_| {
        language
            .inspection_settings()
            .setting("native.check")
            .is_some_and(|setting| !setting.enabled)
    });

    open_inspections(cx, window, "native.check");
    click(cx, window, "settings-inspection-reset");
    draw(cx, window);
    click(cx, window, "settings-ok");
    cx.run_until_parked();
    wait_for_signal(cx, "the reset of the checks", |_| {
        language.inspection_settings().inspections.is_empty()
    });
}

#[gpui::test]
fn a_disabled_check_leaves_the_editor(cx: &mut TestAppContext) {
    let root = project(
        "inspection-editor-off",
        &[("src/Main.txt", "first\nsecond\n")],
    );
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_inspections(vec![check("native.check", "Native check")]);
    language.set_diagnostics(vec![warning(1, "native.check")]);
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the marks of the check", |cx| {
        !marks(cx, window, &path).is_empty()
    });

    open_inspections(cx, window, "native.check");
    assert!(
        has(cx, window, "settings-inspection-0"),
        "the page shows the check"
    );
    click(cx, window, "settings-inspection-toggle-0");
    draw(cx, window);
    click(cx, window, "settings-ok");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        language
            .inspection_settings()
            .setting("native.check")
            .map(|setting| setting.enabled),
        Some(false),
        "the check is off in the language"
    );

    wait_for_signal(cx, "the marks to go away", |cx| {
        marks(cx, window, &path).is_empty()
    });
    assert_eq!(
        language
            .inspection_settings()
            .setting("native.check")
            .map(|setting| setting.enabled),
        Some(false)
    );
}

#[gpui::test]
fn alt_enter_suppresses_the_check_with_a_comment(cx: &mut TestAppContext) {
    let root = project(
        "inspection-suppress",
        &[("src/Main.txt", "first\nsecond\n")],
    );
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_inspections(vec![check("native.check", "Native check")]);
    language.set_diagnostics(vec![warning(1, "native.check")]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the marks of the check", |cx| {
        !marks(cx, window, &path).is_empty()
    });

    click_editor(cx, window, 1, 0);
    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(has(cx, window, "actions-popup"), "Alt+Enter lists the rows");
    press(cx, window, "enter");
    draw(cx, window);

    let text = buffer_of(cx, &editor_of(cx, window, &path));
    assert!(
        text.contains("//noinspection native.check"),
        "the comment is written above the line: {text}"
    );
    wait_for_signal(cx, "the suppressed mark", |cx| {
        marks(cx, window, &path).is_empty()
    });
}

#[gpui::test]
fn alt_enter_disables_the_check_in_the_project(cx: &mut TestAppContext) {
    let root = project("inspection-disable", &[("src/Main.txt", "first\nsecond\n")]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_inspections(vec![check("native.check", "Native check")]);
    language.set_diagnostics(vec![warning(1, "native.check")]);
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the marks of the check", |cx| {
        !marks(cx, window, &path).is_empty()
    });

    click_editor(cx, window, 1, 0);
    press(cx, window, "alt-enter");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    cx.run_until_parked();

    wait_for_signal(cx, "the check to be off", |_| {
        language
            .inspection_settings()
            .setting("native.check")
            .is_some_and(|setting| !setting.enabled)
    });
    assert!(
        fs::read_to_string(workspace::inspections_file(&root))
            .is_ok_and(|text| text.contains("native.check") && text.contains("enabled = false")),
        "the choice is saved in the project"
    );
}

#[gpui::test]
fn inspect_code_hides_a_disabled_check(cx: &mut TestAppContext) {
    let root = project("inspection-inspect-off", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_inspections(vec![check("native.check", "Native check")]);
    language.set_diagnostics(vec![warning(0, "native.check")]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);

    let mut settings = InspectionSettings::default();
    settings.set("native.check", false, None);
    window
        .update(cx, |workspace, _, cx| {
            crate::inspection::apply(workspace, &settings, cx)
        })
        .unwrap();
    cx.run_until_parked();

    press(cx, window, "alt-shift-i");
    draw(cx, window);
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(INSPECTION_TOOL))
            .unwrap(),
        "the results window opens"
    );
    let findings = cx.read(|cx| {
        cx.try_global::<ActiveInspect>()
            .map(|inspect| inspect.0.read(cx).findings().len())
            .unwrap_or(0)
    });
    assert_eq!(findings, 0, "a check that is off gives no findings");
    assert_eq!(active_path(cx, window), Some(path));
}

/// The unused-import problems the language itself reports, through the provider the
/// editor reads. The editor hides the mark of a disabled check at once, so the raw
/// answer is the proof that the running server applied the `.prefs` file.
fn unused_imports(
    runtime: &tokio::runtime::Runtime,
    language: &window_language::RealLanguage,
    path: &std::path::Path,
) -> Vec<Severity> {
    use futures_util::StreamExt;

    let support = language.support();
    let Some(provider) = support.diagnostics() else {
        return Vec::new();
    };
    let uri = crate::window_tests_lang::uri_of(path);
    let entries = runtime.block_on(async {
        let mut replies = lang_api::ask(provider, lang_api::DiagnosticsQuery(uri));
        replies
            .next()
            .await
            .map(|answer| answer.value)
            .unwrap_or_default()
    });
    entries
        .iter()
        .filter(|entry| {
            support
                .inspection_of(entry.code.as_deref(), &entry.message)
                .is_some_and(|info| info.id.ends_with("unusedImport"))
        })
        .map(|entry| entry.severity)
        .collect()
}

/// The setting of a compiler problem of the real server. A new choice goes to a `.prefs`
/// file of its own, and `workspace/didChangeConfiguration` makes the running server read
/// it, so the server is not started again.
#[gpui::test]
#[ignore = "needs the real language server and a corpus project"]
fn the_real_server_takes_the_unused_import_setting(cx: &mut TestAppContext) {
    use crate::window_tests_real::{build_window, wait_ready};
    use window_language::RealLanguage;

    let root = scratch("inspection-real");
    fs::write(
        root.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion>         <groupId>com.example</groupId><artifactId>inspections</artifactId>         <version>1.0</version></project>
",
    )
    .unwrap();
    let source = format!(
        "package com.example;

import {}.util.List;

class Main {{
}}
",
        concat!("ja", "va")
    );
    let path = window_language::write_test_source(&root, "Main", &source);
    let language = RealLanguage::open(root.clone(), scratch("inspection-real-state"))
        .expect("the language server starts");
    let window = build_window(
        cx,
        root,
        &language,
        project_setup(scratch("inspection-real-cache")),
    );
    wait_ready(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the runtime starts");
    wait_for_signal(cx, "the unused import warning", |cx| {
        marks(cx, window, &path)
            .iter()
            .any(|(severity, _)| *severity == Severity::Warning)
            && unused_imports(&runtime, &language, &path).contains(&Severity::Warning)
    });

    let mut settings = InspectionSettings::default();
    settings.set(
        "org.eclipse.jdt.core.compiler.problem.unusedImport",
        false,
        None,
    );
    window
        .update(cx, |workspace, _, cx| {
            crate::inspection::apply(workspace, &settings, cx)
        })
        .unwrap();
    press(cx, window, "ctrl-w");
    open_path(cx, window, &path);
    draw(cx, window);
    wait_for_signal(cx, "the server to drop the warning", |cx| {
        marks(cx, window, &path).is_empty() && unused_imports(&runtime, &language, &path).is_empty()
    });

    settings.set(
        "org.eclipse.jdt.core.compiler.problem.unusedImport",
        true,
        Some(InspectionLevel::Error),
    );
    window
        .update(cx, |workspace, _, cx| {
            crate::inspection::apply(workspace, &settings, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the unused import error", |cx| {
        marks(cx, window, &path)
            .iter()
            .any(|(severity, _)| *severity == Severity::Error)
            && unused_imports(&runtime, &language, &path).contains(&Severity::Error)
    });

    settings.set(
        "org.eclipse.jdt.core.compiler.problem.unusedImport",
        true,
        None,
    );
    window
        .update(cx, |workspace, _, cx| {
            crate::inspection::apply(workspace, &settings, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the warning to come back", |cx| {
        marks(cx, window, &path)
            .iter()
            .any(|(severity, _)| *severity == Severity::Warning)
            && unused_imports(&runtime, &language, &path).contains(&Severity::Warning)
    });
}
