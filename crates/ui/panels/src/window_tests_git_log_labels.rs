//! The labels of a commit row of the Git Log without a window: they sit after the subject
//! and never paint over the author and the date. A commit whose labels do not fit shows the
//! first one by priority and a `+N` badge; a commit with one label draws it whole. The
//! repositories live in scratch folders, and the tests wait for the layout to settle.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{TOOLTIP_DELAY, clear_shown_tooltips, px, shown_tooltips};
use vcs::GitRunner;
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, open_window, press, scratch, wait_for_settled_bounds};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

/// The six labels of the first test: five branches on its commit and main, which is HEAD.
const SIX_LABELS: [&str; 6] = [
    "bugfix/pet-validation",
    "feature/owner-search",
    "feature/visits/calendar",
    "feature/visits/reminders",
    "main",
    "release/2026.11",
];

/// The pills of the first row by their positions in the labels of the commit, as the row
/// names them.
const FIRST_ROW_LABELS: [&str; 6] = [
    "log-ref-0-0",
    "log-ref-0-1",
    "log-ref-0-2",
    "log-ref-0-3",
    "log-ref-0-4",
    "log-ref-0-5",
];

/// A repository with one commit on `main`. The tests add the labels they need to it.
fn repository(name: &str) -> (PathBuf, GitRunner) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("pet.txt"), "pet\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "pet validation"]).unwrap();
    (root, git)
}

/// The Log opened the way Alt+9 opens it, with its first row drawn.
fn open_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
}

/// Hovers the middle of an element until its tooltip shows, and returns the text of it.
fn hover_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> String {
    clear_shown_tooltips();
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    drop(visual);
    // The first wait shows the tooltip, and the second one lets the tooltip become ready.
    for _ in 0..2 {
        draw(cx, window);
        cx.executor()
            .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
        cx.run_until_parked();
    }
    draw(cx, window);
    shown_tooltips()
        .into_iter()
        .last()
        .map(|(text, _)| text)
        .unwrap_or_else(|| panic!("{selector} at {bounds:?} shows no tooltip"))
}

#[ui_kit::gpui::test]
fn a_commit_with_six_long_labels_keeps_them_clear_of_the_author_and_the_date(
    cx: &mut TestAppContext,
) {
    let (root, git) = repository("git-log-labels-six");
    for branch in SIX_LABELS.into_iter().filter(|name| *name != "main") {
        git.run(&root, ["branch", branch]).unwrap();
    }
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-author-0");

    // main is HEAD, so its label comes first; the other five do not fit and collapse.
    let mut drawn = Vec::new();
    for selector in FIRST_ROW_LABELS {
        if has_selector(cx, window, selector) {
            drawn.push(bounds_of(cx, window, selector));
        }
    }
    assert_eq!(drawn.len(), 1, "one of the six labels is drawn");
    assert!(
        has_selector(cx, window, "log-ref-head-0"),
        "the label drawn is the one of HEAD"
    );
    assert!(
        has_selector(cx, window, "log-ref-more-0"),
        "the rest of the labels collapse into a +N badge"
    );
    let badge = bounds_of(cx, window, "log-ref-more-0");
    let labels = bounds_of(cx, window, "log-labels-0");
    let text = bounds_of(cx, window, "log-text-0");
    let subject = bounds_of(cx, window, "log-subject-0");
    let author = bounds_of(cx, window, "log-author-0");
    let date = bounds_of(cx, window, "log-date-0");

    let shown = drawn
        .iter()
        .map(|rect| ("a label", *rect))
        .chain([("the +N badge", badge)]);
    for (what, rect) in shown {
        assert!(
            rect.right() <= labels.right(),
            "{what} {rect:?} leaves the labels {labels:?}"
        );
        assert!(
            rect.right() <= author.left(),
            "{what} {rect:?} paints over the author {author:?}"
        );
        assert!(
            rect.right() <= date.left(),
            "{what} {rect:?} paints over the date {date:?}"
        );
    }
    assert!(
        subject.size.width > px(0.),
        "the subject of the commit is visible: {subject:?}"
    );
    assert!(
        subject.right() <= labels.left(),
        "the subject ends before the labels: {subject:?} and {labels:?}"
    );
    assert!(
        labels.size.width <= text.size.width / 3. + px(1.),
        "the labels take at most a third of the text column: {labels:?} of {text:?}"
    );
    assert!(
        text.right() <= author.left(),
        "the text column ends before the author: {text:?} and {author:?}"
    );
    assert!(
        author.right() <= date.left(),
        "the author ends before the date: {author:?} and {date:?}"
    );

    // Hovering the badge names every label of the commit.
    let tooltip = hover_text(cx, window, "log-ref-more-0");
    for name in SIX_LABELS {
        assert!(
            tooltip.contains(name),
            "the tooltip of +N names {name}: {tooltip}"
        );
    }
}

#[ui_kit::gpui::test]
fn a_commit_with_one_label_draws_it_whole_without_a_count(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-log-labels-one");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-author-0");

    assert!(
        has_selector(cx, window, "log-ref-0-0"),
        "the label of main is drawn"
    );
    assert!(
        has_selector(cx, window, "log-ref-head-0"),
        "the label drawn is the one of HEAD"
    );
    assert!(
        !has_selector(cx, window, "log-ref-more-0"),
        "one label has no +N badge"
    );
    let pill = bounds_of(cx, window, "log-ref-0-0");
    let labels = bounds_of(cx, window, "log-labels-0");
    let text = bounds_of(cx, window, "log-text-0");
    let author = bounds_of(cx, window, "log-author-0");
    assert!(
        pill.size.width > px(0.),
        "the label has its width: {pill:?}"
    );
    assert!(
        pill.right() <= labels.right(),
        "the label stays in the labels {labels:?}: {pill:?}"
    );
    assert!(
        pill.right() <= author.left(),
        "the label paints over the author {author:?}: {pill:?}"
    );
    assert!(
        labels.size.width < text.size.width / 3.,
        "the share of the text column does not cut the label: {labels:?} of {text:?}"
    );
}
