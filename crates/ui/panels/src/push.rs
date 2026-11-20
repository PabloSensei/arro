//! The Push window of Ctrl+Shift+K: the commits of the branch on their way on the left,
//! the files of one of them on the right, the remote and the branch they go to, Push tags
//! and Force Push with a lease. Nothing is sent until Push is pressed.

use std::collections::HashSet;
use std::path::Path;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Commit, PushRequest};
use workspace::{FileIcons, Workspace};

use crate::change_tree::{self, FOLDER_ICON, TreeNode};
use crate::git::{GitState, short_commit};

const WIDTH: f32 = 780.;
/// The branch and the remote branch the user last pushed to wait for this long while the
/// window reads the repository.
const CONTEXT: &str = "PushWindow";

/// The commits of one branch that Push sends.
struct PushBranch {
    name: String,
    commits: Vec<Commit>,
}

/// What the window reads before it shows the commits.
struct OpenedPush {
    branch: String,
    upstream: Option<String>,
    remotes: Vec<String>,
    commits: Vec<Commit>,
}

pub(crate) struct PushWindow {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    focus: FocusHandle,
    scroll_commits: ScrollHandle,
    scroll_files: ScrollHandle,
    branches: Vec<PushBranch>,
    remotes: Vec<String>,
    remote: String,
    destination: TextField,
    upstream: Option<String>,
    current: String,
    selected_branch: usize,
    selected_commit: usize,
    files: Vec<String>,
    files_loading: bool,
    /// The keys of the folders of the files that are folded.
    collapsed: HashSet<String>,
    tags: bool,
    force_confirm: bool,
    force_menu: bool,
    remote_menu: bool,
    loading: bool,
    editing_destination: bool,
    error: Option<String>,
}

impl PushWindow {
    pub(crate) fn open(
        git: Entity<GitState>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let handle = window.window_handle();
        let view = cx.new(|cx| Self {
            git,
            workspace: weak,
            window: handle,
            focus: cx.focus_handle(),
            scroll_commits: ScrollHandle::new(),
            scroll_files: ScrollHandle::new(),
            branches: Vec::new(),
            remotes: Vec::new(),
            remote: String::new(),
            destination: TextField::default(),
            upstream: None,
            current: String::new(),
            selected_branch: 0,
            selected_commit: 0,
            files: Vec::new(),
            files_loading: false,
            collapsed: HashSet::new(),
            tags: false,
            force_confirm: false,
            force_menu: false,
            remote_menu: false,
            loading: true,
            editing_destination: false,
            error: None,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        // The overlay opens under an update of the Git state, whose read would fail
        // here: the first read of the repository waits for the end of that update.
        cx.defer(move |cx| {
            view.update(cx, |this, cx| this.reload(cx));
        });
    }

    /// Reads the branch, the remotes and the commits the remote does not have.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.git.read(cx).repo() else {
            self.loading = false;
            self.error = Some("No Git repository".to_string());
            return;
        };
        self.loading = true;
        self.error = None;
        let job = cx.background_executor().spawn(async move {
            let status = repo.status()?;
            let remotes = repo.remotes()?;
            let commits = repo.outgoing_commits(200)?;
            Ok::<_, std::io::Error>(OpenedPush {
                branch: status.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                upstream: status.upstream.clone(),
                remotes,
                commits,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(opened) => this.opened(opened, cx),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn opened(&mut self, opened: OpenedPush, cx: &mut Context<Self>) {
        let (remote, destination) =
            default_target(&opened.branch, opened.upstream.as_deref(), &opened.remotes);
        self.current = opened.branch.clone();
        self.upstream = opened.upstream;
        self.remotes = opened.remotes;
        self.remote = remote;
        self.destination = TextField::from_text(destination);
        self.branches = (!opened.commits.is_empty())
            .then_some(PushBranch {
                name: opened.branch,
                commits: opened.commits,
            })
            .into_iter()
            .collect();
        self.selected_branch = 0;
        self.selected_commit = 0;
        self.load_files(cx);
    }

    /// Reads the files of the chosen commit for the right pane.
    fn load_files(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self
            .branches
            .get(self.selected_branch)
            .and_then(|branch| branch.commits.get(self.selected_commit))
        else {
            self.files.clear();
            return;
        };
        let id = commit.id.clone();
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        self.files_loading = true;
        let job = cx
            .background_executor()
            .spawn(async move { repo.commit_files(&id).map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.files_loading = false;
                match result {
                    Ok(files) => this.files = files,
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_commit(&mut self, branch: usize, commit: usize, cx: &mut Context<Self>) {
        if self.selected_branch == branch && self.selected_commit == commit {
            return;
        }
        self.selected_branch = branch;
        self.selected_commit = commit;
        self.files.clear();
        self.load_files(cx);
        cx.notify();
    }

    /// Closes the window from a click or a key press. GPUI takes a window out of its slot
    /// while it dispatches an event of that window, so an update of the window from its own
    /// listener fails with "window not found": the listener passes its window instead.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// Closes the window when no event of it runs, as when the remote has the commits.
    fn close_later(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        self.window
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            })
            .ok();
    }

    /// Sends the current branch as the window shows it. The git call runs off the UI
    /// thread; the window closes when the remote has the commits.
    fn push(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let destination = self.destination.text.trim().to_string();
        if self.remotes.is_empty() {
            self.error = Some("The repository has no remote".to_string());
            cx.notify();
            return;
        }
        if destination.is_empty() {
            self.error = Some("Write the branch to push to".to_string());
            cx.notify();
            return;
        }
        if self.current.is_empty() || self.current == "HEAD" {
            self.error = Some("Check out a branch to push".to_string());
            cx.notify();
            return;
        }
        let target = format!("{}/{}", self.remote, destination);
        let set_upstream = self.upstream.as_deref() != Some(target.as_str());
        let request = PushRequest {
            remote: self.remote.clone(),
            branch: self.current.clone(),
            destination,
            set_upstream,
            tags: self.tags,
            force,
        };
        self.error = None;
        let me = cx.weak_entity();
        self.git.update(cx, |git, cx| {
            git.push_to(
                request,
                move |_, cx| {
                    me.update(cx, |this, cx| this.close_later(cx)).ok();
                },
                cx,
            )
        });
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            if self.force_confirm {
                self.force_confirm = false;
                cx.notify();
            } else {
                self.close(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        // Enter ends the edit of the destination; otherwise it presses Push, the primary button.
        if key == "enter" {
            if self.editing_destination {
                self.editing_destination = false;
                cx.notify();
            } else {
                self.push(false, cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.editing_destination && self.destination.key_with_clipboard(event, false, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn checkbox(&self, checked: bool, theme: &Theme) -> Div {
        div()
            .flex_none()
            .size(px(14.))
            .rounded(px(3.))
            .border_1()
            .border_color(if checked {
                theme.accent
            } else {
                theme.text_secondary
            })
            .when(checked, |this| this.bg(theme.accent))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(checked, |this| {
                        this.child(icon_sized("general/check", 10., theme))
                    }),
            )
    }

    fn commits_panel(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut flat = 0usize;
        for (branch_index, branch) in self.branches.iter().enumerate() {
            rows.push(
                div()
                    .debug_selector(move || format!("push-branch-{branch_index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .text_color(theme.text_secondary)
                    .font_weight(FontWeight::MEDIUM)
                    .child(branch.name.clone())
                    .into_any_element(),
            );
            for (commit_index, commit) in branch.commits.iter().enumerate() {
                let selected =
                    self.selected_branch == branch_index && self.selected_commit == commit_index;
                let selector = flat;
                flat += 1;
                rows.push(
                    div()
                        .id(("push-commit", selector))
                        .debug_selector(move || format!("push-commit-{selector}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(ROW_PADDING_X))
                        .rounded(px(ROW_RADIUS))
                        .when(selected, |this| this.bg(theme.selection_inactive))
                        .hover({
                            let hover = theme.selection_hover;
                            move |style| style.bg(hover)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_commit(branch_index, commit_index, cx)
                        }))
                        .child(
                            div()
                                .w(px(60.))
                                .flex_none()
                                .font_family(MONO_FONT)
                                .text_color(theme.link)
                                .child(short_commit(&commit.id)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(commit.subject.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text_secondary)
                                .child(commit.author.clone()),
                        )
                        .into_any_element(),
                );
            }
        }
        let empty = rows.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Commits to push"),
            )
            .child(
                scrollable(
                    "push-commits",
                    &self.scroll_commits,
                    div()
                        .id("push-commits")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_commits)
                        .children(rows)
                        .when(empty && !self.loading, |this| {
                            this.px(px(SPACE_M))
                                .py(px(SPACE_M))
                                .text_color(theme.text_secondary)
                                .child("Nothing to push")
                        })
                        .when(self.loading, |this| {
                            this.px(px(SPACE_M))
                                .py(px(SPACE_M))
                                .text_color(theme.text_secondary)
                                .child("Reading the repository...")
                        }),
                    theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn files_panel(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icons = self
            .workspace
            .read_with(cx, |workspace, _| workspace.icons().clone())
            .unwrap_or_default();
        let entries: Vec<(&str, usize)> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.as_str(), index))
            .collect();
        let nodes = change_tree::tree_rows(&entries, "push", 0, &self.collapsed);
        let rows: Vec<AnyElement> = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| self.file_row(position, node, &icons, theme, cx))
            .collect();
        let empty = rows.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .border_l_1()
            .border_color(theme.toolbar_border)
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Files"),
            )
            .child(
                scrollable(
                    "push-files",
                    &self.scroll_files,
                    div()
                        .id("push-files")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_files)
                        .children(rows)
                        .when(empty && !self.files_loading, |this| {
                            this.px(px(SPACE_M))
                                .py(px(SPACE_M))
                                .text_color(theme.text_secondary)
                                .child("No files")
                        }),
                    theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    /// One row of the tree of the files. A folder opens and closes on a click; a file shows
    /// the icon of its type, as the project tree does.
    fn file_row(
        &self,
        position: usize,
        node: &TreeNode,
        icons: &FileIcons,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icon = match node.file {
            Some(_) => icons.icon_for(Path::new(&node.path)),
            None => FOLDER_ICON,
        };
        let row = tree_row(
            TreeRow {
                id: ("push-tree-row", position).into(),
                depth: node.depth,
                label: node.label.clone().into(),
                hint: None,
                icon,
                expandable: node.file.is_none(),
                open: node.open,
                selected: false,
            },
            theme,
        );
        match node.file {
            Some(index) => row
                .debug_selector(move || format!("push-file-{index}"))
                .into_any_element(),
            None => {
                let path = node.path.clone();
                let key = node.key.clone();
                row.debug_selector(move || format!("push-folder-{path}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_folder(&key, cx)))
                    .into_any_element()
            }
        }
    }

    /// Opens the folder of the tree that is closed, and closes the one that is open.
    fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        cx.notify();
    }

    fn controls(&self, theme: &Theme, label_width: f32, cx: &mut Context<Self>) -> AnyElement {
        let remote = div()
            .id("push-remote")
            .debug_selector(|| "push-remote".to_string())
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.popup_border)
            .cursor_pointer()
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(if self.remote.is_empty() {
                "No remote".to_string()
            } else {
                format!("{}  ▾", self.remote)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.remote_menu = !this.remote_menu;
                cx.notify();
            }));
        let branch = div()
            .id("push-destination")
            .debug_selector(|| "push-destination".to_string())
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if self.editing_destination {
                theme.accent
            } else {
                theme.popup_border
            })
            .overflow_hidden()
            .on_click(cx.listener(|this, _, _, cx| {
                this.editing_destination = true;
                cx.notify();
            }))
            .children(text_field_children(
                FieldMouse::new(cx, |this| Some(&mut this.destination)),
                &self.destination,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                self.editing_destination,
            ));
        // The upstream of the branch stands on the row of the Branch field, small and grey.
        let upstream = div()
            .debug_selector(|| "push-upstream".to_string())
            .flex_none()
            .ml(px(SPACE_M))
            .text_color(theme.text_secondary)
            .text_size(px(TEXT_SMALL))
            .child(match &self.upstream {
                Some(upstream) => format!("Upstream: {upstream}"),
                None => "No upstream yet".to_string(),
            });
        let branch_row = div().flex().items_center().child(branch).child(upstream);
        let tags = div()
            .id("push-tags")
            .debug_selector(|| "push-tags".to_string())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .cursor_pointer()
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(self.checkbox(self.tags, theme))
            .child("Push tags")
            .on_click(cx.listener(|this, _, _, cx| {
                this.tags = !this.tags;
                cx.notify();
            }));
        let remotes: Vec<AnyElement> = if self.remote_menu {
            self.remotes
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let chosen = name.clone();
                    div()
                        .id(("push-remote-choice", index))
                        .debug_selector(move || format!("push-remote-{index}"))
                        .flex()
                        .items_center()
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(SPACE_M))
                        .rounded(px(ROW_RADIUS))
                        .when(self.remote == *name, |this| {
                            this.bg(theme.selection_inactive)
                        })
                        .hover({
                            let hover = theme.selection_hover;
                            move |style| style.bg(hover)
                        })
                        .child(name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remote = chosen.clone();
                            this.remote_menu = false;
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(SPACE_M))
            .py(px(SPACE_S))
            .child(form_row(label_width, "Remote:", remote, theme))
            .child(form_row(label_width, "Branch:", branch_row, theme))
            .child(form_aligned(label_width, tags))
            .children(remotes)
            .into_any_element()
    }

    fn buttons(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let push = dialog_button("push-go", "push-go".to_owned(), "Push", true, true, theme)
            .on_click(cx.listener(|this, _, _, cx| this.push(false, cx)));
        // The menu of Force Push is an extra button: it stands at the left edge of the row.
        let force = dialog_button(
            "push-force",
            "push-force".to_owned(),
            "Force Push  ▾",
            false,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.force_menu = !this.force_menu;
            cx.notify();
        }));
        let force_menu = self.force_menu.then(|| {
            div()
                .id("push-force-item")
                .debug_selector(|| "push-force-item".to_string())
                .flex()
                .items_center()
                .flex_none()
                .h(px(ROW_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(ROW_RADIUS))
                .border_1()
                .border_color(theme.popup_border)
                .bg(theme.popup_bg)
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .child("Force Push")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.force_menu = false;
                    this.force_confirm = true;
                    cx.notify();
                }))
        });
        let cancel = dialog_button(
            "push-cancel",
            "push-cancel".to_owned(),
            "Cancel",
            false,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let warning = self.force_confirm.then(|| {
            div()
                .debug_selector(|| "push-force-warning".to_string())
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .gap(px(SPACE_S))
                .text_color(theme.stripe_warning)
                .child("Force Push replaces the remote history.")
                .child(
                    dialog_button(
                        "push-force-confirm",
                        "push-force-confirm".to_owned(),
                        "Force Push",
                        false,
                        true,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.push(true, cx))),
                )
        });
        // The bar keeps the padding of the dialog from its right and bottom edges, as the
        // other dialogs do; the warning of a forced push stands on its own line above the row.
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_S))
            .px(px(DIALOG_PADDING))
            .pt(px(SPACE_M))
            .pb(px(DIALOG_PADDING))
            .children(force_menu)
            .children(warning)
            .child(
                DialogButtons::new()
                    .extra(force)
                    .primary(push)
                    .other(cancel)
                    .into_row(),
            )
            .into_any_element()
    }
}

/// The remote and the branch Push chooses before the user changes them: the upstream of
/// the branch, or the first remote and the name of the branch.
fn default_target(branch: &str, upstream: Option<&str>, remotes: &[String]) -> (String, String) {
    if let Some(upstream) = upstream {
        if let Some((remote, name)) = upstream.split_once('/') {
            return (remote.to_string(), name.to_string());
        }
        if !remotes.is_empty() {
            return (remotes[0].clone(), upstream.to_string());
        }
    }
    let remote = remotes
        .iter()
        .find(|name| name.as_str() == "origin")
        .or_else(|| remotes.first())
        .cloned()
        .unwrap_or_default();
    (remote, branch.to_string())
}

impl Render for PushWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let label_width = form_label_width(window, &["Remote:", "Branch:"]);
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.65;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.14;
        let error = self.error.clone();
        let shell = surface(&theme)
            .id("push-dialog")
            .debug_selector(|| "push-dialog".to_string())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.controls(&theme, label_width, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.commits_panel(&theme, window, cx))
                    .child(self.files_panel(&theme, window, cx)),
            )
            .children(error.map(|error| {
                div()
                    .debug_selector(|| "push-error".to_string())
                    .flex_none()
                    .px(px(SPACE_M))
                    .py(px(SPACE_S))
                    .text_color(theme.error)
                    .child(error)
            }))
            .child(self.buttons(&theme, cx));
        // The cross dismisses through the window of the click: a nested update of the
        // window that is handling the click would fail, and the dialog would stay open.
        let workspace = self.workspace.clone();
        let close = cx.listener(move |_, _: &ClickEvent, window, cx| {
            workspace
                .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                .ok();
        });
        dialog_window(
            DialogSpec {
                key: "push",
                title: "Push".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (480., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::default_target;

    #[test]
    fn the_upstream_names_the_remote_and_the_branch() {
        assert_eq!(
            default_target("feature", Some("origin/main"), &["origin".to_string()]),
            ("origin".to_string(), "main".to_string())
        );
        assert_eq!(
            default_target(
                "feature",
                None,
                &["upstream".to_string(), "origin".to_string()]
            ),
            ("origin".to_string(), "feature".to_string())
        );
        assert_eq!(
            default_target("feature", None, &["upstream".to_string()]),
            ("upstream".to_string(), "feature".to_string())
        );
    }
}
