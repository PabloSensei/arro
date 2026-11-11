//! The scrollbar of the editor and its marks, and the problem widget in its corner.

use std::time::Duration;

use editor_view::StripeKind;
use lang_api::{DiagnosticEntry, Severity, TextPoint, TextSpan};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{TOOLTIP_DELAY, clear_shown_tooltips, shown_tooltips};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, press, type_text, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

fn diagnostic(severity: Severity, line: u32, message: &str) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint { line, character: 0 },
            end: TextPoint { line, character: 4 },
        },
        severity,
        message: message.to_owned(),
        unnecessary: false,
    }
}

fn lines(count: usize) -> String {
    (0..count).map(|line| format!("line {line}\n")).collect()
}

/// Clicks a point of the window.
fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: ui_kit::Point<ui_kit::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, Modifiers::default());
    cx.run_until_parked();
}

/// Hovers the middle of an element and returns the tooltip it shows.
fn hover(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> (String, Option<String>) {
    clear_shown_tooltips();
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    shown_tooltips()
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("{selector} shows no tooltip"))
}

#[ui_kit::gpui::test]
fn an_error_stripe_is_on_the_bar_and_its_click_goes_to_the_line(cx: &mut TestAppContext) {
    let root = project("scrollbar-stripe", &[("src/Main.txt", &lines(200))]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![diagnostic(Severity::Error, 50, "the error")]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the diagnostics", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    draw(cx, window);

    let stripe = view
        .read_with(cx, |view, _| view.stripe_bounds(StripeKind::Error, 50))
        .expect("the error has a mark on the bar");
    assert!(stripe.size.height > ui_kit::px(0.), "the mark is painted");
    click_at(cx, window, stripe.center());
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (50, 0),
        "a click on the mark puts the caret on the error"
    );
}

#[ui_kit::gpui::test]
fn the_problem_widget_counts_and_leads_to_the_next_problem(cx: &mut TestAppContext) {
    let root = project("scrollbar-widget", &[("src/Main.txt", &lines(200))]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![
        diagnostic(Severity::Error, 3, "first"),
        diagnostic(Severity::Error, 8, "second"),
        diagnostic(Severity::Warning, 12, "third"),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the diagnostics", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    draw(cx, window);

    let summary = view.read_with(cx, |view, _| view.problem_summary());
    assert_eq!((summary.errors, summary.warnings), (2, 1));
    assert!(!summary.analyzing, "the server has answered");

    let (text, _) = hover(cx, window, "editor-problem-widget");
    assert_eq!(text, "2 errors, 1 warning");
    assert!(
        view.read_with(cx, |view, _| view.stripe_bounds(StripeKind::Warning, 12))
            .is_some(),
        "the warning has a mark on the bar"
    );

    press(cx, window, "escape");
    let bounds = bounds_of(cx, window, "editor-problem-widget");
    click_at(cx, window, bounds.center());
    assert_eq!(
        view.read_with(cx, |view, _| view.caret().0),
        3,
        "the widget goes to the first error"
    );
    let bounds = bounds_of(cx, window, "editor-problem-widget");
    click_at(cx, window, bounds.center());
    assert_eq!(
        view.read_with(cx, |view, _| view.caret().0),
        8,
        "the widget goes to the next problem"
    );
}

#[ui_kit::gpui::test]
fn find_marks_come_with_the_search_and_leave_with_it(cx: &mut TestAppContext) {
    let text: String = (0..200)
        .map(|line| {
            if line % 10 == 0 {
                format!("line {line} needle\n")
            } else {
                format!("line {line}\n")
            }
        })
        .collect();
    let root = project("scrollbar-find", &[("src/Main.txt", &text)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-f");
    type_text(cx, window, "needle");
    draw(cx, window);
    let found = view.read_with(cx, |view, _| {
        view.stripe_kinds()
            .into_iter()
            .filter(|(kind, line)| *kind == StripeKind::Find && *line == 100)
            .count()
    });
    assert_eq!(found, 1, "the matches have marks on the bar");

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !view
            .read_with(cx, |view, _| view.stripe_kinds())
            .iter()
            .any(|(kind, _)| *kind == StripeKind::Find),
        "closing the search takes the marks away"
    );
}
