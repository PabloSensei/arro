//! The TODO tool window: `TODO` and `FIXME` in the text of the project.

use std::path::{Path, PathBuf};

use editor_view::Semantic;
use ui_kit::*;
use workspace::{Settings, TODO_TOOL, Workspace};

use crate::items::{HeaderButton, Item, ItemsPanel, PanelRow};

/// Folders that hold no source of the user, whatever the project: the application adds the
/// output folders of its build tools.
const SKIPPED: &[&str] = &[".git", ".idea", "node_modules"];
/// A bigger file is data, not text a person wrote.
const MAX_FILE_BYTES: u64 = 1 << 20;
const KEYWORDS: [&str; 2] = ["TODO", "FIXME"];

/// More folders the scan skips, from the application.
#[derive(Clone, Default)]
pub struct SkippedFolders(pub Vec<String>);

impl Global for SkippedFolders {}

/// The panel of the window, for scripts.
pub struct ActiveTodo(pub Entity<ItemsPanel>);

impl Global for ActiveTodo {}

/// The marks of the last scan, so a grouping rule can rebuild the list without reading
/// the project again.
pub struct TodoData {
    pub root: PathBuf,
    pub marks: Vec<(PathBuf, Mark)>,
}

impl Global for TodoData {}

/// One mark in a text: the line from zero, the column in characters, the word and what
/// follows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    pub column: usize,
    pub keyword: &'static str,
    pub text: String,
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Finds the first `TODO` or `FIXME` of every line that stands as a word of its own.
pub fn find_marks(text: &str) -> Vec<Mark> {
    let mut marks = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let found = KEYWORDS.iter().filter_map(|keyword| {
            let mut from = 0;
            while let Some(at) = content[from..].find(keyword) {
                let start = from + at;
                let end = start + keyword.len();
                let before = content[..start].chars().next_back();
                let after = content[end..].chars().next();
                if !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char) {
                    return Some((start, *keyword, end));
                }
                from = end;
            }
            None
        });
        if let Some((start, keyword, end)) = found.min_by_key(|(start, ..)| *start) {
            let rest = content[end..].trim_start_matches([':', ' ', '\t', '-']);
            marks.push(Mark {
                line,
                column: content[..start].chars().count(),
                keyword,
                text: rest.trim().to_string(),
            });
        }
    }
    marks
}

fn skipped(name: &str, more: &[String]) -> bool {
    let hidden = name.starts_with('.') && name.len() > 1 && name != ".github";
    SKIPPED.contains(&name) || hidden || more.iter().any(|folder| folder == name)
}

/// Reads the text files under `root` and lists the marks. Blocks: not for the UI thread.
pub fn scan(root: &Path, more_skipped: &[String]) -> Vec<(PathBuf, Mark)> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !skipped(&name, more_skipped) {
                    folders.push(entry.path());
                }
                continue;
            }
            let small = entry
                .metadata()
                .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
            if !small {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for mark in find_marks(&text) {
                found.push((entry.path(), mark));
            }
        }
    }
    found.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    found
}

#[cfg(test)]
fn items_of(root: &Path, marks: Vec<(PathBuf, Mark)>) -> Vec<Item> {
    marks
        .into_iter()
        .map(|(path, mark)| {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let hint = format!("{}:{}", relative.display(), mark.line + 1);
            let label = if mark.text.is_empty() {
                mark.keyword.to_string()
            } else {
                format!("{} {}", mark.keyword, mark.text)
            };
            Item {
                path,
                line: mark.line,
                column: mark.column,
                label,
                hint: hint.replace('\\', "/"),
                icon: "status/info",
            }
        })
        .collect()
}

pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<ItemsPanel> {
    let panel = ItemsPanel::create("TODO", TODO_TOOL, "No TODO or FIXME found.", workspace, cx);
    panel.update(cx, |_, cx| {
        cx.observe_global::<Settings>(regroup).detach();
    });
    rescan(&panel, root, cx);
    panel
}

/// Reads the project again in the background and fills the list.
pub fn rescan(panel: &Entity<ItemsPanel>, root: PathBuf, cx: &mut App) {
    let scan_root = root.clone();
    let more = cx
        .try_global::<SkippedFolders>()
        .map_or_else(Vec::new, |skipped| skipped.0.clone());
    let task = cx
        .background_executor()
        .spawn(async move { scan(&scan_root, &more) });
    let panel = panel.downgrade();
    cx.spawn(async move |cx| {
        let marks = task.await;
        cx.update(|cx| {
            cx.set_global(TodoData {
                root: root.clone(),
                marks,
            });
        });
        panel.update(cx, regroup).ok();
    })
    .detach();
}

/// Rebuilds the list with the grouping rules of the settings.
pub fn regroup(panel: &mut ItemsPanel, cx: &mut Context<ItemsPanel>) {
    let Some(data) = cx.try_global::<TodoData>() else {
        return;
    };
    let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
    let root = data.root.clone();
    let mut entries: Vec<(Vec<String>, Item)> = data
        .marks
        .iter()
        .map(|(path, mark)| {
            let item = item_of(&root, path, mark);
            let keys = group_keys(path, &root, &settings, cx);
            (keys, item)
        })
        .collect();
    entries.sort_by(|a, b| {
        (&a.0, &a.1.path, a.1.line, a.1.column).cmp(&(&b.0, &b.1.path, b.1.line, b.1.column))
    });
    let mut items = Vec::with_capacity(entries.len());
    let mut rows = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    for (keys, item) in entries {
        let common = previous
            .iter()
            .zip(&keys)
            .take_while(|(left, right)| left == right)
            .count();
        for (depth, key) in keys.iter().enumerate().skip(common) {
            rows.push(PanelRow::Group {
                label: key.clone(),
                depth,
                icon: "general/folder",
            });
        }
        previous = keys;
        let index = items.len();
        rows.push(PanelRow::Item {
            index,
            depth: previous.len(),
            excluded: false,
        });
        items.push(item);
    }
    let count = items.len();
    panel.set_title(format!("TODO ({count})"), cx);
    let buttons = header_buttons(&settings, panel);
    panel.set_header_buttons(buttons, cx);
    panel.set_view(items, rows, cx);
}

fn group_keys(path: &Path, root: &Path, settings: &Settings, cx: &App) -> Vec<String> {
    let semantic = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone());
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut keys = Vec::new();
    if settings.todo_group_modules {
        let module = semantic
            .as_ref()
            .and_then(|support| support.module_of(path))
            .unwrap_or_else(|| {
                relative
                    .parent()
                    .and_then(|parent| parent.components().next())
                    .map(|first| first.as_os_str().to_string_lossy().into_owned())
                    .or_else(|| {
                        root.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .unwrap_or_default()
            });
        keys.extend(split_name(&module));
    }
    if settings.todo_group_packages {
        let package = semantic
            .as_ref()
            .and_then(|support| support.package_of(path))
            .unwrap_or_else(|| {
                relative
                    .parent()
                    .map(|parent| parent.display().to_string().replace('\\', "/"))
                    .unwrap_or_default()
            });
        if settings.todo_flatten_packages {
            keys.push(package);
        } else {
            keys.extend(split_name(&package));
        }
    }
    keys
}

fn split_name(name: &str) -> Vec<String> {
    name.split(['.', '/', '\\'])
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn header_buttons(settings: &Settings, panel_ref: &ItemsPanel) -> Vec<HeaderButton> {
    let workspace = panel_ref.workspace();
    let button =
        |id, label: &str, icon: &'static str, tooltip: &str, active, change: fn(&mut Settings)| {
            let workspace = workspace.clone();
            let tooltip = tooltip.to_owned();
            HeaderButton {
                id,
                label: label.to_owned(),
                active,
                icon: Some(icon),
                tooltip: Some(tooltip),
                shortcut: None,
                run: std::rc::Rc::new(move |_, cx| {
                    let workspace = workspace.clone();
                    cx.spawn(async move |cx| {
                        workspace
                            .update(cx, |workspace, cx| workspace.update_settings(change, cx))
                            .ok();
                    })
                    .detach();
                }),
            }
        };
    vec![
        button(
            "todo-group-modules",
            "M",
            "nodes/module",
            "Group by Module",
            settings.todo_group_modules,
            |settings| settings.todo_group_modules = !settings.todo_group_modules,
        ),
        button(
            "todo-group-packages",
            "P",
            "nodes/package",
            "Group by Package",
            settings.todo_group_packages,
            |settings| settings.todo_group_packages = !settings.todo_group_packages,
        ),
        button(
            "todo-flatten-packages",
            "C",
            "general/collapseAll",
            "Flatten Packages",
            settings.todo_flatten_packages,
            |settings| settings.todo_flatten_packages = !settings.todo_flatten_packages,
        ),
    ]
}

fn item_of(root: &Path, path: &Path, mark: &Mark) -> Item {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let hint = format!("{}:{}", relative.display(), mark.line + 1);
    let label = if mark.text.is_empty() {
        mark.keyword.to_string()
    } else {
        format!("{} {}", mark.keyword, mark.text)
    };
    Item {
        path: path.to_path_buf(),
        line: mark.line,
        column: mark.column,
        label,
        hint: hint.replace('\\', "/"),
        icon: "status/info",
    }
}

#[cfg(test)]
mod tests {
    use super::{Mark, find_marks, items_of, scan};
    use std::path::{Path, PathBuf};

    #[test]
    fn marks_are_whole_words_with_their_text() {
        let text = "int a; // TODO: fix this\nTODOS are not marks\n  // FIXME - later\nx = METODO;";
        let marks = find_marks(text);
        assert_eq!(
            marks,
            [
                Mark {
                    line: 0,
                    column: 10,
                    keyword: "TODO",
                    text: "fix this".into()
                },
                Mark {
                    line: 2,
                    column: 5,
                    keyword: "FIXME",
                    text: "later".into()
                },
            ]
        );
    }

    #[test]
    fn a_bare_mark_has_no_text_and_the_column_counts_characters() {
        let marks = find_marks("é TODO");
        assert_eq!(marks[0].column, 2);
        assert!(marks[0].text.is_empty());
    }

    #[test]
    fn the_scan_reads_text_files_and_skips_binary_and_build_output() {
        let root = test_support::scratch("todo-scan");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(root.join("src/A.txt"), "ok\n// TODO one\n").unwrap();
        std::fs::write(root.join("out/B.txt"), "TODO hidden\n").unwrap();
        std::fs::write(root.join("bin.dat"), b"TODO\0binary").unwrap();

        let found = scan(&root, &["out".to_string()]);
        let items = items_of(&root, found);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "TODO one");
        assert_eq!(items[0].hint, "src/A.txt:2");
        assert_eq!(items[0].path, PathBuf::from(&items[0].path));
        assert!(Path::new(&items[0].hint).extension().is_some());
    }
}
