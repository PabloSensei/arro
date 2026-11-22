//! The line above a tab of a class whose sources are missing, and the work that fetches
//! or attaches them. The language names the artifact and does the work; this module only
//! draws the line and follows the progress.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ::base::Cancel;
use editor_view::{EditorView, FileBar, Semantic};
use futures_util::StreamExt;
use lang_api::{
    LibraryClassQuery, SourceTextQuery, SourcesEvent, SourcesKind, SourcesRequest, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, Workspace};

use crate::outline::OpenEditors;

/// The command of Find Action and of the tool window.
pub(crate) const DOWNLOAD_ALL: &str = "project.download_all_sources";
/// The command the button of the notification runs.
pub(crate) const CANCEL: &str = "project.cancel_source_download";

/// What the line above one tab says now.
struct Status {
    /// The origin of the class, as the language named it.
    uri: String,
    artifact: String,
    detail: Option<String>,
    has_source: bool,
    phase: Phase,
}

enum Phase {
    Idle,
    Downloading {
        cancel: Cancel,
        done: usize,
        total: usize,
    },
    Failed(String),
}

#[derive(Default)]
struct Statuses(HashMap<PathBuf, Status>);

impl Global for Statuses {}

/// The window the progress line of the whole project goes to.
struct SourceWindow(WeakEntity<Workspace>);

impl Global for SourceWindow {}

/// The token of the request that fetches the sources of the whole project.
#[derive(Default)]
struct Downloads {
    cancel: Option<Cancel>,
}

impl Global for Downloads {}

/// Puts the line above the tabs in place over the row the other windows may already have.
pub(crate) fn install(cx: &mut App) {
    // A second window of the same application shares the state and the row.
    if cx.try_global::<Statuses>().is_none() {
        cx.set_global(Statuses::default());
        cx.set_global(Downloads::default());
        let previous = cx.try_global::<FileBar>().map(|bar| bar.0.clone());
        cx.set_global(FileBar(Rc::new(
            move |path: &Path, window: &mut Window, cx: &mut Context<EditorView>| {
                file_bar(path, window, cx)
                    .or_else(|| previous.as_ref().and_then(|bar| bar(path, window, cx)))
            },
        )));
    }
}

/// Remembers the window the notifications go to and adds the two commands.
pub(crate) fn register(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    cx.set_global(SourceWindow(cx.weak_entity()));
    workspace.register_command(DOWNLOAD_ALL, "Download Sources for All", |_, window, cx| {
        download_all(window, cx);
    });
    workspace.register_command(CANCEL, "Cancel Source Download", |_, _, cx| {
        if let Some(downloads) = cx.try_global::<Downloads>()
            && let Some(cancel) = &downloads.cancel
        {
            cancel.cancel();
        }
    });
}

/// Asks the language what the tab of `path` shows. A file never asks: only a text that
/// came from somewhere else has an origin.
pub(crate) fn tab_opened(path: &Path, cx: &mut App) {
    let Some(uri) = EditorView::source_origin(path, cx) else {
        return;
    };
    let replies = {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            return;
        };
        let Some(provider) = semantic.0.library_class() else {
            return;
        };
        ask(provider, LibraryClassQuery(uri.clone()))
    };
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let mut replies = replies;
        let Some(answer) = replies.next().await else {
            return;
        };
        let Some(class) = answer.value else {
            return;
        };
        let auto = cx.update(|cx| {
            let statuses = cx.default_global::<Statuses>();
            let status = statuses.0.entry(path.clone()).or_insert_with(|| Status {
                uri,
                artifact: class.artifact.clone(),
                detail: class.detail.clone(),
                has_source: class.has_source,
                phase: Phase::Idle,
            });
            status.artifact = class.artifact.clone();
            status.detail = class.detail.clone();
            status.has_source = class.has_source;
            touch(&path, cx);
            cx.try_global::<Settings>()
                .is_some_and(|settings| settings.download_sources_automatically)
        });
        if auto && !class.has_source {
            cx.update(|cx| download(&path, cx));
        }
    })
    .detach();
}

/// Fetches the sources of the class of one tab.
fn download(path: &Path, cx: &mut App) {
    let Some((uri, cancel)) = begin(path, cx) else {
        return;
    };
    let replies = {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            set_failed(path, "The language has no sources to fetch".to_owned(), cx);
            return;
        };
        let Some(provider) = semantic.0.library_sources() else {
            set_failed(path, "The language has no sources to fetch".to_owned(), cx);
            return;
        };
        ask(
            provider,
            SourcesRequest {
                kind: SourcesKind::Class(uri),
                cancel: cancel.clone(),
            },
        )
    };
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let mut replies = replies;
        while let Some(answer) = replies.next().await {
            let event = answer.value;
            cx.update(|cx| class_event(&path, event, cx));
        }
    })
    .detach();
}

/// Attaches the file or folder the user picks.
fn choose(path: &Path, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some("Choose Sources".into()),
    });
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(chosen) = paths.into_iter().next() else {
            return;
        };
        cx.update(|cx| attach(&path, chosen, cx));
    })
    .detach();
}

fn attach(path: &Path, chosen: PathBuf, cx: &mut App) {
    let Some(uri) = cx
        .try_global::<Statuses>()
        .and_then(|statuses| statuses.0.get(path))
        .map(|status| status.uri.clone())
    else {
        return;
    };
    let replies = {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            return;
        };
        let Some(provider) = semantic.0.library_sources() else {
            return;
        };
        ask(
            provider,
            SourcesRequest {
                kind: SourcesKind::Attach { uri, path: chosen },
                cancel: Cancel::new(),
            },
        )
    };
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let mut replies = replies;
        while let Some(answer) = replies.next().await {
            let event = answer.value;
            cx.update(|cx| class_event(&path, event, cx));
        }
    })
    .detach();
}

fn begin(path: &Path, cx: &mut App) -> Option<(String, Cancel)> {
    let cancel = Cancel::new();
    let uri = {
        let statuses = cx.default_global::<Statuses>();
        let status = statuses.0.get_mut(path)?;
        status.phase = Phase::Downloading {
            cancel: cancel.clone(),
            done: 0,
            total: 0,
        };
        status.uri.clone()
    };
    touch(path, cx);
    Some((uri, cancel))
}

fn set_failed(path: &Path, message: String, cx: &mut App) {
    if let Some(status) = cx.default_global::<Statuses>().0.get_mut(path) {
        status.phase = Phase::Failed(message);
    }
    touch(path, cx);
}

fn class_event(path: &Path, event: SourcesEvent, cx: &mut App) {
    let mut refresh = None;
    {
        let statuses = cx.default_global::<Statuses>();
        let Some(status) = statuses.0.get_mut(path) else {
            return;
        };
        match event {
            SourcesEvent::Started { total } => {
                if let Phase::Downloading {
                    cancel,
                    done,
                    total: old,
                } = &mut status.phase
                {
                    *done = 0;
                    *old = total;
                    let _ = cancel;
                } else {
                    status.phase = Phase::Downloading {
                        cancel: Cancel::new(),
                        done: 0,
                        total,
                    };
                }
            }
            SourcesEvent::Progress { done, total, .. } => {
                if let Phase::Downloading {
                    done: current,
                    total: old,
                    ..
                } = &mut status.phase
                {
                    *current = done;
                    *old = total;
                }
            }
            SourcesEvent::Attached { uri } => {
                status.has_source = true;
                status.phase = Phase::Idle;
                refresh = Some(uri);
            }
            SourcesEvent::Missing { .. } => {
                status.phase = Phase::Failed("No repository has it (404)".to_owned());
            }
            SourcesEvent::Failed { message } => {
                status.phase = Phase::Failed(format!("Could not download the sources: {message}"));
            }
            SourcesEvent::Cancelled => status.phase = Phase::Idle,
            SourcesEvent::Summary { .. } => {}
        }
    }
    touch(path, cx);
    if let Some(uri) = refresh {
        refresh_text(path.to_path_buf(), uri, cx);
    }
}

/// Reads the class again once the server has taken its sources. The caret stays on the
/// symbol it was on where the new text still has it.
fn refresh_text(path: PathBuf, uri: String, cx: &mut App) {
    let replies = {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            return;
        };
        let Some(provider) = semantic.0.source_text() else {
            return;
        };
        ask(provider, SourceTextQuery(uri))
    };
    cx.spawn(async move |cx| {
        let mut replies = replies;
        let Some(answer) = replies.next().await else {
            return;
        };
        let text = answer.value;
        if text.is_empty() {
            return;
        }
        cx.update(|cx| {
            if let Some(view) = OpenEditors::get(cx, &path) {
                view.update(cx, |view, cx| view.replace_source_text(text, cx));
            }
        });
    })
    .detach();
}

/// Fetches the sources of every library of the project, with the progress in a
/// notification and a button that stops the work.
pub(crate) fn download_all(_window: &mut Window, cx: &mut App) {
    let cancel = Cancel::new();
    cx.default_global::<Downloads>().cancel = Some(cancel.clone());
    let replies = {
        let Some(semantic) = cx.try_global::<Semantic>() else {
            notify(
                cx,
                NotificationLevel::Warning,
                "Download Sources",
                "The language is not ready",
            );
            return;
        };
        let Some(provider) = semantic.0.library_sources() else {
            notify(
                cx,
                NotificationLevel::Warning,
                "Download Sources",
                "The language has no sources to fetch",
            );
            return;
        };
        ask(
            provider,
            SourcesRequest {
                kind: SourcesKind::All,
                cancel,
            },
        )
    };
    notify_with_cancel(
        cx,
        "Download Sources",
        "Looking for the sources of the libraries of the project",
    );
    cx.spawn(async move |cx| {
        let mut replies = replies;
        while let Some(answer) = replies.next().await {
            let event = answer.value;
            cx.update(|cx| all_event(event, cx));
        }
    })
    .detach();
}

fn all_event(event: SourcesEvent, cx: &mut App) {
    match event {
        SourcesEvent::Started { total } => {
            notify_with_cancel(
                cx,
                "Download Sources",
                &format!("Fetching the sources of {total} libraries"),
            );
        }
        SourcesEvent::Progress { done, total, name } => {
            notify_with_cancel(
                cx,
                "Download Sources",
                &format!("Fetching the sources of {name}: {done} of {total}"),
            );
        }
        SourcesEvent::Summary {
            fetched,
            skipped,
            failed,
        } => {
            cx.default_global::<Downloads>().cancel = None;
            let mut text = format!("Downloaded {fetched} sources");
            if skipped > 0 {
                text.push_str(&format!(", {skipped} already there"));
            }
            if failed > 0 {
                text.push_str(&format!(", {failed} failed"));
            }
            let level = if failed > 0 {
                NotificationLevel::Warning
            } else {
                NotificationLevel::Info
            };
            notify(cx, level, "Download Sources", &text);
        }
        SourcesEvent::Missing { name } => {
            cx.default_global::<Downloads>().cancel = None;
            notify(
                cx,
                NotificationLevel::Warning,
                "Download Sources",
                &format!("No repository has the sources of {name}"),
            );
        }
        SourcesEvent::Failed { message } => {
            cx.default_global::<Downloads>().cancel = None;
            notify(cx, NotificationLevel::Error, "Download Sources", &message);
        }
        SourcesEvent::Cancelled => {
            cx.default_global::<Downloads>().cancel = None;
            notify(
                cx,
                NotificationLevel::Info,
                "Download Sources",
                "The download was stopped",
            );
        }
        SourcesEvent::Attached { .. } => {}
    }
}

fn notify(cx: &mut App, level: NotificationLevel, title: &str, text: &str) {
    let window = cx
        .try_global::<SourceWindow>()
        .map(|window| window.0.clone());
    if let Some(window) = window {
        let (title, text) = (title.to_owned(), text.to_owned());
        // A command runs while the window is being updated; the notification opens
        // after that update is over.
        cx.defer(move |cx| {
            window
                .update(cx, |workspace, cx| match level {
                    NotificationLevel::Info => workspace.notify_user(title, text, cx),
                    NotificationLevel::Warning => workspace.notify_warning(title, text, cx),
                    NotificationLevel::Error => workspace.notify_error(title, text, cx),
                })
                .ok();
        });
    }
}

fn notify_with_cancel(cx: &mut App, title: &str, text: &str) {
    let window = cx
        .try_global::<SourceWindow>()
        .map(|window| window.0.clone());
    if let Some(window) = window {
        let (title, text) = (title.to_owned(), text.to_owned());
        cx.defer(move |cx| {
            window
                .update(cx, |workspace, cx| {
                    workspace.notify_user_with_command(title, text, "Cancel", CANCEL, cx)
                })
                .ok();
        });
    }
}

/// The editor and the window show the new state of the line.
fn touch(path: &Path, cx: &mut App) {
    if let Some(view) = OpenEditors::get(cx, path) {
        view.update(cx, |_, cx| cx.notify());
    }
    let window = cx
        .try_global::<SourceWindow>()
        .map(|window| window.0.clone());
    if let Some(window) = window {
        cx.defer(move |cx| {
            window.update(cx, |_, cx| cx.notify()).ok();
        });
    }
}

/// The line above the text of a tab of a class of a library.
fn file_bar(path: &Path, _window: &mut Window, cx: &mut Context<EditorView>) -> Option<AnyElement> {
    let status = cx.try_global::<Statuses>()?.0.get(path)?;
    if status.has_source {
        return None;
    }
    let theme = *cx.global::<Theme>();
    let (text, detail) = bar_text(status);
    let downloading = matches!(status.phase, Phase::Downloading { .. });
    let mut row = div()
        .id("library-sources-bar")
        .debug_selector(|| "library-sources-bar".to_owned())
        .flex()
        .items_center()
        .flex_none()
        .gap(px(SPACE_M))
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .bg(theme.feedback_bg)
        .border_b_1()
        .border_color(theme.feedback_border)
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text);
    if let Some(detail) = detail {
        row = row.child(
            div()
                .text_color(theme.text_disabled)
                .child(format!("(bytecode {detail})")),
        );
    }
    if !downloading {
        let download_path = path.to_path_buf();
        row = row.child(link(
            "library-sources-download",
            "Download Sources",
            theme,
            move |cx| download(&download_path, cx),
        ));
        let choose_path = path.to_path_buf();
        row = row.child(link(
            "library-sources-choose",
            "Choose Sources…",
            theme,
            move |cx| choose(&choose_path, cx),
        ));
    } else if let Phase::Downloading { cancel, .. } = &status.phase {
        let cancel = cancel.clone();
        row = row.child(link("library-sources-cancel", "Cancel", theme, move |_| {
            cancel.cancel()
        }));
    }
    Some(row.into_any_element())
}

fn bar_text(status: &Status) -> (String, Option<String>) {
    match &status.phase {
        Phase::Idle => (
            format!("Sources not found for {}", status.artifact),
            status.detail.clone(),
        ),
        Phase::Downloading { done, total, .. } => {
            let text = if *total > 0 {
                format!(
                    "Downloading the sources of {}: {done} of {total}",
                    status.artifact
                )
            } else {
                format!("Downloading the sources of {}", status.artifact)
            };
            (text, None)
        }
        Phase::Failed(message) => (
            format!("Sources not found for {}. {message}", status.artifact),
            None,
        ),
    }
}

/// What the line above the tab of `path` says, for the window tests.
#[cfg(test)]
pub(crate) fn bar_text_at(path: &Path, cx: &App) -> Option<String> {
    let status = cx.try_global::<Statuses>()?.0.get(path)?;
    (!status.has_source).then(|| bar_text(status).0)
}

/// A link-style button of the line. Its name is required: a test finds it by it.
fn link(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    run: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let (link, text) = (theme.link, theme.text);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .occlude()
        .text_color(link)
        .cursor_pointer()
        .hover(move |style| style.text_color(text))
        .child(label)
        .on_click(move |_, _, cx| run(cx))
}
