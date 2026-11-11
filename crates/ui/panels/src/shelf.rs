//! Shelves: patches of chosen files kept in the state folder of the project, and the
//! dialog that puts chosen files of a shelf back.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
#[cfg(test)]
use vcs::patch_files;
use workspace::Workspace;

use crate::project_state::project_key;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// One shelf: the name the user sees, the files the patch touches and the patch itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shelf {
    pub id: String,
    pub name: String,
    pub files: Vec<String>,
    pub patch: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Meta {
    name: String,
    files: Vec<String>,
}

/// The shelves of one project, one folder in the state folder of the application.
#[derive(Clone, Debug)]
pub struct ShelfStore {
    dir: PathBuf,
}

impl ShelfStore {
    pub fn new(state_dir: &Path, root: &Path) -> Self {
        Self {
            dir: state_dir.join("shelf").join(project_key(root)),
        }
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Shelf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut shelves = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<Meta>(&text) else {
                continue;
            };
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let patch = std::fs::read(self.patch_path(id)).unwrap_or_default();
            shelves.push(Shelf {
                id: id.to_string(),
                name: meta.name,
                files: meta.files,
                patch,
            });
        }
        shelves.sort_by(|a, b| b.id.cmp(&a.id));
        shelves
    }

    /// A name no shelf of the project has yet: `Changes`, `Changes (2)`, and so on.
    pub fn unique_name(&self, name: &str) -> String {
        let name = name.trim();
        let name = if name.is_empty() { "Changes" } else { name };
        let names: Vec<String> = self.list().into_iter().map(|shelf| shelf.name).collect();
        if !names.iter().any(|known| known == name) {
            return name.to_string();
        }
        for number in 2.. {
            let candidate = format!("{name} ({number})");
            if !names.iter().any(|known| known == &candidate) {
                return candidate;
            }
        }
        name.to_string()
    }

    pub fn create(&self, name: &str, files: Vec<String>, patch: Vec<u8>) -> std::io::Result<Shelf> {
        std::fs::create_dir_all(&self.dir)?;
        let id = self.next_id();
        let meta = Meta {
            name: name.to_string(),
            files,
        };
        // The patch first: a shelf is listed by its meta file, so it never shows without one.
        std::fs::write(self.patch_path(&id), &patch)?;
        std::fs::write(
            self.meta_path(&id),
            serde_json::to_string(&meta).unwrap_or_default(),
        )?;
        Ok(Shelf {
            id,
            name: meta.name,
            files: meta.files,
            patch,
        })
    }

    pub fn rename(&self, id: &str, name: &str) -> std::io::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.meta_path(id);
        let text = std::fs::read_to_string(&path)?;
        let mut meta: Meta = serde_json::from_str(&text).unwrap_or(Meta {
            name: name.to_string(),
            files: Vec::new(),
        });
        meta.name = name.to_string();
        std::fs::write(path, serde_json::to_string(&meta).unwrap_or_default())
    }

    pub fn delete(&self, id: &str) {
        let _ = std::fs::remove_file(self.meta_path(id));
        let _ = std::fs::remove_file(self.patch_path(id));
    }

    fn next_id(&self) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_nanos())
            .unwrap_or_default();
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        format!("{nanos:020}-{serial:04}")
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn patch_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.patch"))
    }
}

/// The named parts of a patch, in the order of the patch.
#[cfg(test)]
pub fn patch_sections(patch: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(patch);
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") && !sections.is_empty() {
            let names = patch_files(sections.last().unwrap().1.as_bytes());
            if let Some(section) = sections.last_mut()
                && let Some(name) = names.first()
            {
                section.0 = name.clone();
            }
        }
        if line.starts_with("diff --git ") || sections.is_empty() {
            sections.push((String::new(), String::new()));
        }
        if let Some(section) = sections.last_mut() {
            section.1.push_str(line);
        }
    }
    for (name, text) in sections.iter_mut() {
        if name.is_empty() {
            *name = patch_files(text.as_bytes())
                .first()
                .cloned()
                .unwrap_or_default();
        }
    }
    sections
}

type Apply = Rc<dyn Fn(Vec<String>, bool, &mut App)>;

/// The dialog of Unshelve: the files of one shelf with ticks, and whether the shelf goes
/// away after it is applied.
pub struct UnshelveDialog {
    shelf: String,
    files: Vec<(String, bool)>,
    selected: usize,
    delete_after: bool,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    apply: Apply,
    scroll: ScrollHandle,
}

impl UnshelveDialog {
    pub fn show(
        shelf: &Shelf,
        apply: impl Fn(Vec<String>, bool, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let shelf_name = shelf.name.clone();
        let mut files: Vec<(String, bool)> = shelf
            .files
            .iter()
            .map(|file| (file.clone(), true))
            .collect();
        files.sort();
        let view = cx.new(|cx| Self {
            shelf: shelf_name,
            files,
            selected: 0,
            delete_after: false,
            focus: cx.focus_handle(),
            workspace: weak,
            apply: Rc::new(apply),
            scroll: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn chosen(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(file, _)| file.clone())
            .collect()
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some((_, chosen)) = self.files.get_mut(index) {
            *chosen = !*chosen;
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.selected = (self.selected + 1).min(self.files.len().saturating_sub(1));
                cx.stop_propagation();
                cx.notify();
            }
            "space" | " " => {
                self.toggle(self.selected, cx);
                cx.stop_propagation();
            }
            "enter" => {
                self.unpack(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn unpack(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.chosen();
        if files.is_empty() {
            return;
        }
        let delete_after = self.delete_after;
        (self.apply)(files, delete_after, cx);
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for UnshelveDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let width = px(460.);
        let left = (window.viewport_size().width - width) / 2.;
        let top = window.viewport_size().height * 0.22;
        let rows: Vec<AnyElement> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, (file, chosen))| {
                let checked = *chosen;
                let file = file.clone();
                div()
                    .id(("unshelve-file", index))
                    .debug_selector(move || format!("unshelve-file-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(self.selected == index, |this| {
                        this.bg(theme.selection_inactive)
                    })
                    .hover({
                        let hover = theme.selection_hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        this.toggle(index, cx);
                    }))
                    .child(
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
                                        this.child(
                                            div()
                                                .size(px(6.))
                                                .rounded(px(1.))
                                                .bg(theme.over_accent),
                                        )
                                    }),
                            ),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(file))
                    .into_any_element()
            })
            .collect();
        let delete = self.delete_after;
        let body = surface(&theme)
            .id("unshelve-dialog")
            .debug_selector(|| "unshelve-dialog".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Unshelve {}", self.shelf)),
            )
            .child(
                scrollable(
                    "unshelve-files",
                    &self.scroll,
                    div()
                        .id("unshelve-files")
                        .flex()
                        .flex_col()
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_none(),
            )
            .child(
                div()
                    .id("unshelve-delete")
                    .debug_selector(|| "unshelve-delete".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.delete_after = !this.delete_after;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(14.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if delete {
                                theme.accent
                            } else {
                                theme.text_secondary
                            })
                            .when(delete, |this| this.bg(theme.accent)),
                    )
                    .child("Delete shelf after apply"),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("unshelve-cancel")
                            .debug_selector(|| "unshelve-cancel".to_owned())
                            .px(px(12.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.popup_border)
                            .hover({
                                let hover = theme.hover;
                                move |style| style.bg(hover)
                            })
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        div()
                            .id("unshelve-apply")
                            .debug_selector(|| "unshelve-apply".to_owned())
                            .px(px(12.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.accent)
                            .text_color(theme.over_accent)
                            .child("Unshelve")
                            .on_click(cx.listener(|this, _, window, cx| this.unpack(window, cx))),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("unshelve", body))
    }
}

#[cfg(test)]
mod tests {
    use super::patch_sections;

    #[test]
    fn the_patch_is_split_by_file() {
        let patch = b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n+one\n\
diff --git a/dir/b.txt b/dir/b.txt\n--- a/dir/b.txt\n+++ b/dir/b.txt\n+two\n";

        let sections = patch_sections(patch);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "a.txt");
        assert_eq!(sections[1].0, "dir/b.txt");
        assert!(sections[1].1.contains("+two"));
    }
}
