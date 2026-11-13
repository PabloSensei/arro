//! Run marks in the gutter of an editor: where the tests of a file start, where a
//! program can be started, how the last run went for them, and which one is under the
//! caret.

use editor_view::{EditorView, RunMark, RunMarkState};
use runner::{LaunchPoint, RunConfig, TestRequest, TestSite, TestState};
use ui_kit::*;
use workspace::{DEBUG_TOOL, RUN_TOOL, Workspace};

use crate::run::RunPanel;
use crate::run_configs::RunConfigDialog;
use crate::script::ActiveRunPanel;

const READY: (u32, u32) = (0x5fb865, 0x208a3c);
const RUNNING: (u32, u32) = (0xf2c55c, 0xb59b1b);
const FAILED: (u32, u32) = (0xf75464, 0xc7222d);

fn mark_color((dark, light): (u32, u32), is_dark: bool) -> Hsla {
    rgb(if is_dark { dark } else { light }).into()
}

fn panel_of(cx: &App) -> Option<Entity<RunPanel>> {
    cx.try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
}

fn sites_in(view: &EditorView, text: &str, panel: &RunPanel) -> Vec<TestSite> {
    (panel.test_sites())(text, view.path())
}

/// Puts the marks of the tests and of the places that start a program in the gutter.
pub(crate) fn refresh(view: &mut EditorView, cx: &mut Context<EditorView>) {
    let Some(panel) = panel_of(cx) else {
        return;
    };
    let Some(text) = view.text() else {
        return;
    };
    let dark = cx.global::<Theme>().dark;
    let panel = panel.read(cx);
    let sites = if panel.can_test() {
        sites_in(view, &text, panel)
    } else {
        Vec::new()
    };
    let mut marks: Vec<RunMark> = sites
        .iter()
        .map(|site| {
            let state = panel.test_state(&site.suite, site.case.as_deref());
            let (state, colors) = match state {
                Some(TestState::Running) => (RunMarkState::Running, RUNNING),
                Some(TestState::Passed) => (RunMarkState::Passed, READY),
                Some(TestState::Failed { .. }) => (RunMarkState::Failed, FAILED),
                _ => (RunMarkState::Ready, READY),
            };
            RunMark {
                line: site.line.saturating_sub(1),
                state,
                color: mark_color(colors, dark),
            }
        })
        .collect();
    // A line that holds both a test and a program keeps the test mark.
    marks.extend(
        panel
            .launch_points(&text, view.path())
            .into_iter()
            .map(|point| RunMark {
                line: point.line.saturating_sub(1),
                state: RunMarkState::Ready,
                color: mark_color(READY, dark),
            }),
    );
    marks.sort_by_key(|mark| mark.line);
    marks.dedup_by_key(|mark| mark.line);
    view.set_run_marks(marks, cx);
}

/// The test or class whose mark is on this line, from zero.
pub(crate) fn request_at_line(view: &EditorView, line: usize, cx: &App) -> Option<TestRequest> {
    let panel = panel_of(cx)?;
    let text = view.text()?;
    let sites = sites_in(view, &text, panel.read(cx));
    let site = sites.iter().find(|site| site.line as usize == line + 1)?;
    Some(request_of(site))
}

fn request_of(site: &TestSite) -> TestRequest {
    TestRequest {
        suite: Some(site.suite.clone()),
        case: site.case.clone(),
        ..TestRequest::default()
    }
}

fn points_in(view: &EditorView, text: &str, panel: &RunPanel) -> Vec<LaunchPoint> {
    panel.launch_points(text, view.path())
}

/// The program that can be started on this line, from zero.
pub(crate) fn launch_at_line(view: &EditorView, line: usize, cx: &App) -> Option<RunConfig> {
    let panel = panel_of(cx)?;
    let text = view.text()?;
    points_in(view, &text, panel.read(cx))
        .into_iter()
        .find(|point| point.line as usize == line + 1)
        .map(|point| point.config)
}

/// The program whose type or method holds the caret.
pub(crate) fn launch_at_caret(view: &EditorView, cx: &App) -> Option<RunConfig> {
    let panel = panel_of(cx)?;
    let text = view.text()?;
    let caret = view.caret_line()?;
    let points = points_in(view, &text, panel.read(cx));
    if points.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut best: Option<&LaunchPoint> = None;
    for point in &points {
        let start = point.line.saturating_sub(1) as usize;
        let (first, last) = (with_annotations(&lines, start), block_end(&lines, start));
        let inside = (first..=last).contains(&caret);
        if inside && best.is_none_or(|best| point.line > best.line) {
            best = Some(point);
        }
    }
    best.map(|point| point.config.clone())
}

/// The menu of a place that starts a program: Run, Debug and the dialog of the
/// configuration it would use.
pub(crate) fn launch_menu(
    panel: Entity<RunPanel>,
    config: RunConfig,
    workspace: WeakEntity<Workspace>,
) -> Vec<MenuEntry> {
    let name = config.name.clone();
    let platform = keymap::platform();
    let window_hint = |action: &str| {
        ::workspace::window_key(action, platform).map(|keys| keymap::shortcut_label(keys, platform))
    };
    let run_config = config.clone();
    let run_panel = panel.clone();
    let run_workspace = workspace.clone();
    let mut run = MenuEntry::new(format!("Run '{name}'"), move |window, cx| {
        run_panel.update(cx, |panel, cx| {
            panel.choose_configuration(run_config.clone(), cx);
            panel.run(cx);
        });
        run_workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(RUN_TOOL, window, cx)
            })
            .ok();
    })
    .icon("run/run");
    if let Some(hint) = window_hint("workspace::Run") {
        run = run.shortcut(hint);
    }
    let debug_config = config.clone();
    let debug_panel = panel.clone();
    let debug_workspace = workspace.clone();
    let mut debug = MenuEntry::new(format!("Debug '{name}'"), move |window, cx| {
        debug_panel.update(cx, |panel, cx| {
            panel.choose_configuration(debug_config.clone(), cx);
            panel.debug(window, cx);
        });
        debug_workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(DEBUG_TOOL, window, cx)
            })
            .ok();
    })
    .icon("run/debug");
    if let Some(hint) = window_hint("workspace::Debug") {
        debug = debug.shortcut(hint);
    }
    let modify_config = config;
    let modify_panel = panel.clone();
    let modify_workspace = workspace;
    let modify = MenuEntry::new("Modify Run Configuration...", move |window, cx| {
        modify_workspace
            .update(cx, |workspace, cx| {
                RunConfigDialog::show_config(
                    modify_panel.clone(),
                    modify_config.clone(),
                    workspace,
                    window,
                    cx,
                )
            })
            .ok();
    });
    vec![run, debug, modify]
}

/// A click on a mark: a program opens its menu, a test runs as it always did.
pub(crate) fn run_mark_clicked(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if launch_at_line(view.read(cx), line, cx).is_some() {
        show_launch_menu(view, line, workspace, window, cx);
        return;
    }
    if let Some(request) = request_at_line(view.read(cx), line, cx) {
        run_request(request, workspace, window, cx);
    }
}

/// Alt+click on a mark: a program starts under the debugger, a test as it always did.
pub(crate) fn debug_mark_clicked(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(config) = launch_at_line(view.read(cx), line, cx) {
        if let Some(panel) = panel_of(cx) {
            panel.update(cx, |panel, cx| {
                panel.choose_configuration(config, cx);
                panel.debug(window, cx);
            });
        }
        workspace.show_tool(DEBUG_TOOL, window, cx);
        return;
    }
    if let Some(request) = request_at_line(view.read(cx), line, cx) {
        debug_request(request, workspace, window, cx);
    }
}

/// The menu of the place on this line, at the pointer.
pub(crate) fn show_launch_menu(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(config) = launch_at_line(view.read(cx), line, cx) else {
        return;
    };
    let Some(panel) = panel_of(cx) else {
        return;
    };
    let entries = launch_menu(panel, config, cx.weak_entity());
    workspace.show_menu(window.mouse_position(), entries, cx);
}

/// The innermost test or class around the caret.
fn request_at_caret(view: &EditorView, cx: &App) -> Result<TestRequest, String> {
    let (Some(panel), Some(text), Some(caret)) = (panel_of(cx), view.text(), view.caret_line())
    else {
        return Err("Open a file with tests first".into());
    };
    let panel = panel.read(cx);
    if !panel.can_test() {
        return Err("The project has no build tool that runs tests".into());
    }
    let sites = sites_in(view, &text, panel);
    if sites.is_empty() {
        return Err("No tests in this file".into());
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut best: Option<&TestSite> = None;
    for site in &sites {
        let start = site.line.saturating_sub(1) as usize;
        let (first, last) = (with_annotations(&lines, start), block_end(&lines, start));
        let inside = (first..=last).contains(&caret);
        if inside && best.is_none_or(|best| site.line > best.line) {
            best = Some(site);
        }
    }
    let classes: Vec<&TestSite> = sites.iter().filter(|s| s.case.is_none()).collect();
    let alone = (classes.len() == 1).then(|| classes[0]);
    best.or(alone)
        .map(request_of)
        .ok_or_else(|| "The caret is not in a test or a test class".into())
}

/// Annotation lines right above a declaration belong to it.
fn with_annotations(lines: &[&str], start: usize) -> usize {
    let mut first = start;
    while first > 0
        && lines
            .get(first - 1)
            .is_some_and(|l| l.trim_start().starts_with('@'))
    {
        first -= 1;
    }
    first
}

/// The line where the first `{` at or after `start` closes. Strings, characters and
/// comments are skipped; a block that never closes ends at the last line.
fn block_end(lines: &[&str], start: usize) -> usize {
    let (mut depth, mut opened, mut comment) = (0usize, false, false);
    for (index, line) in lines.iter().enumerate().skip(start) {
        let chars: Vec<char> = line.chars().collect();
        let mut at = 0;
        while at < chars.len() {
            let next = chars.get(at + 1).copied();
            match chars[at] {
                '*' if comment && next == Some('/') => {
                    comment = false;
                    at += 1;
                }
                _ if comment => {}
                '/' if next == Some('/') => break,
                '/' if next == Some('*') => {
                    comment = true;
                    at += 1;
                }
                quote @ ('"' | '\'') => {
                    at += 1;
                    while at < chars.len() && chars[at] != quote {
                        at += if chars[at] == '\\' { 2 } else { 1 };
                    }
                }
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' if opened => {
                    depth -= 1;
                    if depth == 0 {
                        return index;
                    }
                }
                _ => {}
            }
            at += 1;
        }
    }
    lines.len().saturating_sub(1)
}

/// Shows the Run window and runs the tests in it.
pub(crate) fn run_request(
    request: TestRequest,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.show_tool(RUN_TOOL, window, cx);
    if let Some(panel) = panel_of(cx) {
        panel.update(cx, |panel, cx| panel.run_tests(request, cx));
    }
}

/// Shows the Debug window and runs the tests in it under the debugger.
pub(crate) fn debug_request(
    request: TestRequest,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.show_tool(DEBUG_TOOL, window, cx);
    if let Some(panel) = panel_of(cx) {
        panel.update(cx, |panel, cx| panel.debug_tests(request, window, cx));
    }
}

/// Ctrl+Shift+F10: the program under the caret, else the test under it or its class.
pub(crate) fn run_at_caret(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let view = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok());
    if let Some(view) = &view
        && let Some(config) = launch_at_caret(view.read(cx), cx)
    {
        if let Some(panel) = panel_of(cx) {
            panel.update(cx, |panel, cx| {
                panel.choose_configuration(config, cx);
                panel.run(cx);
            });
        }
        workspace.show_tool(RUN_TOOL, window, cx);
        return;
    }
    let request = match view {
        Some(view) => request_at_caret(view.read(cx), cx),
        None => Err("Open a file with tests first".to_owned()),
    };
    match request {
        Ok(request) => run_request(request, workspace, window, cx),
        Err(message) => workspace.notify_user("Run", message, cx),
    }
}

/// Runs the failed tests of the last run again.
pub(crate) fn rerun_failed(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.show_tool(RUN_TOOL, window, cx);
    if let Some(panel) = panel_of(cx) {
        panel.update(cx, |panel, cx| panel.rerun_failed(cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{block_end, with_annotations};

    const SOURCE: &str = "class A {\n    @Test\n    void one() {\n        String s = \"}\";\n        // }\n    }\n\n    void helper() {\n    }\n}\n";

    #[test]
    fn a_block_ends_where_its_braces_balance() {
        let lines: Vec<&str> = SOURCE.lines().collect();

        assert_eq!(block_end(&lines, 2), 5);
        assert_eq!(block_end(&lines, 7), 8);
        assert_eq!(block_end(&lines, 0), 9);
    }

    #[test]
    fn annotations_belong_to_the_method_below() {
        let lines: Vec<&str> = SOURCE.lines().collect();

        assert_eq!(with_annotations(&lines, 2), 1);
        assert_eq!(with_annotations(&lines, 7), 7);
    }
}
