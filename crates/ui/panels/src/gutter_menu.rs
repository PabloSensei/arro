//! The menus of the gutter and of the blame column: the handful of rows IDEA puts on a
//! right click in the line numbers, and the view options of the annotations.

use editor_view::EditorView;
use keymap::editor as actions;
use ui_kit::*;
use workspace::{AnnotateNames, Settings, Workspace};

use crate::debug::ActiveDebug;

/// A row of Options | View of the blame menu: the part of the label it switches and, for
/// the names, the style it chooses.
#[derive(Clone, Copy, Debug)]
enum ViewOption {
    Author,
    Date,
    Commit,
    Names(AnnotateNames),
}

impl ViewOption {
    fn apply(self, settings: &mut Settings) {
        match self {
            ViewOption::Author => settings.annotate_author = !settings.annotate_author,
            ViewOption::Date => settings.annotate_date = !settings.annotate_date,
            ViewOption::Commit => settings.annotate_commit = !settings.annotate_commit,
            ViewOption::Names(names) => settings.annotate_names = names,
        }
    }
}

/// `✓ Name` for a switch that is on, as the menus of the window write one.
fn checked(label: &str, on: bool) -> SharedString {
    if on {
        format!("✓ {label}").into()
    } else {
        label.to_owned().into()
    }
}

/// A row that runs a command of the workspace.
fn command_entry(
    label: SharedString,
    workspace: &WeakEntity<Workspace>,
    command: &'static str,
) -> MenuEntry {
    let workspace = workspace.clone();
    MenuEntry::new(label, move |window, cx| {
        workspace
            .update(cx, |workspace, cx| {
                workspace.run_command(command, window, cx)
            })
            .ok();
    })
}

/// A row that changes the settings and reads the blame of the open columns again, so that
/// the labels follow the choice at once.
fn settings_entry(
    label: SharedString,
    workspace: &WeakEntity<Workspace>,
    option: ViewOption,
) -> MenuEntry {
    let workspace = workspace.clone();
    MenuEntry::new(label, move |_, cx| {
        workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(|settings| option.apply(settings), cx)
            })
            .ok();
        if let Some(git) = cx.try_global::<crate::git::ActiveGit>() {
            let git = git.0.clone();
            git.update(cx, |git, cx| git.refresh(cx));
        }
    })
}

/// The menu of a right click in the line numbers at `line`, counted from zero.
pub(crate) fn show_gutter(
    view: Entity<EditorView>,
    workspace: WeakEntity<Workspace>,
    position: Point<Pixels>,
    line: u32,
    cx: &mut App,
) {
    let path = view.read(cx).path().to_path_buf();
    let number = line + 1;
    let settings = cx.global::<Settings>().clone();
    let debug = cx.try_global::<ActiveDebug>().map(|panel| panel.0.clone());
    let existing = debug
        .as_ref()
        .and_then(|panel| panel.read(cx).breakpoint_enabled(&path, number));
    let annotate = view.read(cx).has_annotations();
    let bookmark = view.read(cx).bookmark_at_line(line as usize);

    let mut groups: Vec<Vec<MenuEntry>> = Vec::new();
    // Breakpoints: on a line that already has one, the rows of IDEA, else the three ways
    // to put a new one there.
    let mut breakpoints = Vec::new();
    if let Some(enabled) = existing {
        if let Some(panel) = &debug {
            let panel = panel.clone();
            let file = path.clone();
            breakpoints.push(MenuEntry::new("Remove Breakpoint", move |_, cx| {
                panel.update(cx, |panel, cx| {
                    panel.remove_breakpoint(file.clone(), number, cx)
                });
            }));
        }
        if let Some(panel) = &debug {
            let panel = panel.clone();
            let file = path.clone();
            breakpoints.push(MenuEntry::new("Edit Breakpoint...", move |window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.edit_breakpoint_at(file.clone(), number, window, cx)
                });
            }));
        }
        if let Some(panel) = &debug {
            let panel = panel.clone();
            let file = path.clone();
            if enabled {
                breakpoints.push(MenuEntry::new("Disable Breakpoint", move |_, cx| {
                    panel.update(cx, |panel, cx| {
                        panel.disable_breakpoint(file.clone(), number, cx)
                    });
                }));
            }
        }
    } else if let Some(panel) = &debug {
        let add = panel.clone();
        let file = path.clone();
        breakpoints.push(MenuEntry::new("Add Breakpoint", move |_, cx| {
            add.update(cx, |panel, cx| {
                panel.toggle_breakpoint(file.clone(), number, cx)
            });
        }));
        let conditional = panel.clone();
        let file = path.clone();
        breakpoints.push(MenuEntry::new(
            "Add Conditional Breakpoint...",
            move |window, cx| {
                conditional.update(cx, |panel, cx| {
                    panel.add_conditional_breakpoint(file.clone(), number, window, cx)
                });
            },
        ));
        let logging = panel.clone();
        let file = path.clone();
        breakpoints.push(MenuEntry::new(
            "Add Logging Breakpoint...",
            move |window, cx| {
                logging.update(cx, |panel, cx| {
                    panel.add_logging_breakpoint(file.clone(), number, window, cx)
                });
            },
        ));
    }
    groups.push(breakpoints);

    // Annotate with Git Blame, with the checkmark of the column that is open.
    let command = if annotate {
        crate::git::CLOSE_ANNOTATIONS_COMMAND
    } else {
        crate::git::ANNOTATE_COMMAND
    };
    groups.push(vec![command_entry(
        checked("Annotate with Git Blame", annotate),
        &workspace,
        command,
    )]);

    // Bookmarks of the clicked line.
    let mut bookmarks = Vec::new();
    match bookmark {
        Some(_) => {
            let target = view.clone();
            let f11 = shortcut_of(cx, &workspace, actions::TOGGLE_BOOKMARK);
            let entry = MenuEntry::new("Remove Bookmark", move |_, cx| {
                target.update(cx, |view, cx| {
                    view.toggle_bookmark_at_line(line as usize, None, cx)
                });
            });
            bookmarks.push(match f11 {
                Some(f11) => entry.shortcut(f11),
                None => entry,
            });
        }
        None => {
            let target = view.clone();
            let f11 = shortcut_of(cx, &workspace, actions::TOGGLE_BOOKMARK);
            let entry = MenuEntry::new("Add Bookmark", move |_, cx| {
                target.update(cx, |view, cx| {
                    view.toggle_bookmark_at_line(line as usize, None, cx)
                });
            });
            bookmarks.push(match f11 {
                Some(f11) => entry.shortcut(f11),
                None => entry,
            });
            let target = view.clone();
            let ctrl_f11 = shortcut_of(cx, &workspace, actions::BOOKMARK_MNEMONIC);
            let entry = MenuEntry::new("Add Mnemonic Bookmark...", move |_, cx| {
                target.update(cx, |view, cx| {
                    view.await_mnemonic_bookmark_at(line as usize, cx)
                });
            });
            bookmarks.push(match ctrl_f11 {
                Some(ctrl_f11) => entry.shortcut(ctrl_f11),
                None => entry,
            });
        }
    }
    groups.push(bookmarks);

    // Soft-Wrap: the switch of the whole window, and the page of its settings.
    let mut wrap = Vec::new();
    wrap.push(command_entry(
        checked("Soft-Wrap", settings.soft_wrap),
        &workspace,
        keymap::editor::TOGGLE_SOFT_WRAP,
    ));
    let open = workspace.clone();
    wrap.push(MenuEntry::new(
        "Configure Soft Wraps...",
        move |window, cx| {
            open.update(cx, |workspace, cx| {
                crate::settings::SettingsView::show_at(workspace, window, cx, Some("soft_wrap"))
            })
            .ok();
        },
    ));
    groups.push(wrap);

    // View | Appearance, the switches of the editor that IDEA offers there.
    groups.push(vec![MenuEntry::submenu(
        "Appearance",
        vec![
            command_entry(
                checked("Show Line Numbers", settings.show_line_numbers),
                &workspace,
                workspace::TOGGLE_LINE_NUMBERS,
            ),
            command_entry(
                checked("Show Gutter Icons", settings.show_gutter_icons),
                &workspace,
                workspace::TOGGLE_GUTTER_ICONS,
            ),
            command_entry(
                checked("Show Indent Guides", settings.show_indent_guides),
                &workspace,
                workspace::TOGGLE_INDENT_GUIDES,
            ),
        ],
    )]);

    let open = workspace.clone();
    groups.push(vec![MenuEntry::new(
        "Configure Gutter Icons...",
        move |window, cx| {
            open.update(cx, |workspace, cx| {
                crate::settings::SettingsView::show_at(
                    workspace,
                    window,
                    cx,
                    Some("show_gutter_icons"),
                )
            })
            .ok();
        },
    )]);

    // A request file keeps the command to run it from the gutter, as before.
    if ::http_client::is_request_file(&path) {
        let runner = workspace.clone();
        let clicked = view.clone();
        groups.push(vec![
            MenuEntry::new("Run All Requests", move |window, cx| {
                runner
                    .update(cx, |workspace, cx| {
                        crate::http::run_all(&clicked, workspace, window, cx)
                    })
                    .ok();
            })
            .icon("run/run"),
        ]);
    }

    let entries = joined(groups);
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        })
        .ok();
}

/// The menu of a right click on a blame label at `line`, counted from zero.
pub(crate) fn show_blame(
    view: Entity<EditorView>,
    workspace: WeakEntity<Workspace>,
    position: Point<Pixels>,
    line: u32,
    cx: &mut App,
) {
    let path = view.read(cx).path().to_path_buf();
    let settings = cx.global::<Settings>().clone();
    let note = view.read(cx).blame_note_at_line(line as usize);
    let commit = note
        .as_ref()
        .filter(|note| note.has_commit())
        .map(|note| note.commit.clone());
    let git = cx
        .try_global::<crate::git::ActiveGit>()
        .map(|git| git.0.clone());
    let revision = git
        .as_ref()
        .and_then(|git| git.read(cx).annotated_revision(&path).map(str::to_owned))
        .or_else(|| commit.clone());

    let names = [
        AnnotateNames::LastName,
        AnnotateNames::FirstName,
        AnnotateNames::FullName,
        AnnotateNames::Initials,
        AnnotateNames::Email,
    ];
    let names_rows = names
        .into_iter()
        .map(|style| {
            settings_entry(
                checked(style.label(), settings.annotate_names == style),
                &workspace,
                ViewOption::Names(style),
            )
        })
        .collect();
    let mut rows = vec![command_entry(
        "Close Annotations".into(),
        &workspace,
        crate::git::CLOSE_ANNOTATIONS_COMMAND,
    )];
    rows.push(MenuEntry::separator());
    rows.push(MenuEntry::submenu(
        "Options",
        vec![MenuEntry::submenu(
            "View",
            vec![
                settings_entry(
                    checked("Author", settings.annotate_author),
                    &workspace,
                    ViewOption::Author,
                ),
                settings_entry(
                    checked("Date", settings.annotate_date),
                    &workspace,
                    ViewOption::Date,
                ),
                settings_entry(
                    checked("Commit", settings.annotate_commit),
                    &workspace,
                    ViewOption::Commit,
                ),
                MenuEntry::submenu("Names", names_rows),
            ],
        )],
    ));
    rows.push(MenuEntry::separator());

    match (git.clone(), revision.clone()) {
        (Some(git), Some(revision)) => {
            let target = view.clone();
            let target_path = path.clone();
            rows.push(MenuEntry::new(
                "Annotate Previous Revision",
                move |_, cx| {
                    git.update(cx, |git, cx| {
                        git.annotate_previous_revision(
                            target_path.clone(),
                            target.clone(),
                            Some(revision.clone()),
                            cx,
                        )
                    });
                },
            ));
        }
        _ => rows.push(MenuEntry::disabled("Annotate Previous Revision")),
    }

    match (git, commit.clone()) {
        (Some(git), Some(commit)) => {
            let workspace = workspace.clone();
            let path = path.clone();
            let target = commit.clone();
            rows.push(MenuEntry::new("Show Diff", move |window, cx| {
                crate::diff_view::DiffView::open_commit(
                    git.clone(),
                    workspace.clone(),
                    vec![path.clone()],
                    0,
                    target.clone(),
                    window,
                    cx,
                );
            }));
        }
        _ => rows.push(MenuEntry::disabled("Show Diff")),
    }

    match commit.clone() {
        Some(commit) => {
            let copied = commit.clone();
            rows.push(MenuEntry::new("Copy Revision Number", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
            }));
            let workspace = workspace.clone();
            rows.push(MenuEntry::new("Show in Git Log", move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        crate::git_log::show_commit(&commit, workspace, window, cx)
                    })
                    .ok();
            }));
        }
        None => {
            rows.push(MenuEntry::disabled("Copy Revision Number"));
            rows.push(MenuEntry::disabled("Show in Git Log"));
        }
    }

    workspace
        .update(cx, |workspace, cx| workspace.show_menu(position, rows, cx))
        .ok();
}

/// The rows of every group with a separator between the groups.
fn joined(groups: Vec<Vec<MenuEntry>>) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    let mut first = true;
    for group in groups {
        if group.is_empty() {
            continue;
        }
        if !first {
            entries.push(MenuEntry::separator());
        }
        first = false;
        entries.extend(group);
    }
    entries
}

/// The key of an action of the editor, as the window labels it.
fn shortcut_of(
    cx: &App,
    workspace: &WeakEntity<Workspace>,
    action: &'static str,
) -> Option<SharedString> {
    workspace
        .read_with(cx, |workspace, _| workspace.shortcut_of_key(action))
        .ok()
        .flatten()
        .map(Into::into)
}
