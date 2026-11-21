//! The TODO tool window: the patterns of the settings in the text of the project, such as
//! `TODO` and `FIXME`. The window keeps the marks of the whole project and shows a part
//! of them: by template, by the scope of the project or of the open file.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use editor_view::Semantic;
use ui_kit::*;
use workspace::{Settings, TODO_TOOL, Workspace};

use crate::git::ActiveGit;
use crate::items::{HeaderButton, Item, ItemsPanel, PanelRow, PanelTab};
use crate::script::ActiveRunPanel;

/// Folders that hold no source of the user, whatever the project: the application adds the
/// output folders of its build tools.
const SKIPPED: &[&str] = &[".git", ".idea", "node_modules"];
/// A bigger file is data, not text a person wrote.
const MAX_FILE_BYTES: u64 = 1 << 20;
const MODULE_ICON: &str = "nodes/module";
const PACKAGE_ICON: &str = "nodes/package";
const FOLDER_ICON: &str = "nodes/folder";
/// The icon of a mark, as the TODO window of IDEA shows it.
const MATCH_ICON: &str = "toolwindows/todo";
/// The icon of a file whose type the project does not name.
const FILE_ICON: &str = "fileTypes/text";
/// The patterns of a project that never opened the Settings window.
#[cfg(test)]
const DEFAULT_KEYWORDS: [&str; 2] = ["TODO", "FIXME"];

/// More folders the scan skips, from the application.
#[derive(Clone, Default)]
pub struct SkippedFolders(pub Vec<String>);

impl Global for SkippedFolders {}

/// The panel of the window, for scripts.
pub struct ActiveTodo(pub Entity<ItemsPanel>);

impl Global for ActiveTodo {}

/// What part of the project the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TodoScope {
    #[default]
    Project,
    CurrentFile,
    ChangedFiles,
}

impl TodoScope {
    fn label(self) -> &'static str {
        match self {
            TodoScope::Project => "Project",
            TodoScope::CurrentFile => "Current File",
            TodoScope::ChangedFiles => "Changed Files",
        }
    }
}

/// The filter of the window: one template of the settings, and the scope of the list.
#[derive(Clone, Debug, Default)]
pub struct TodoFilter {
    pub template: Option<String>,
    pub scope: TodoScope,
}

impl Global for TodoFilter {}

/// The marks of the last scan, so a grouping rule or a scope filter can rebuild the list
/// without reading the project again.
pub struct TodoData {
    pub root: PathBuf,
    pub marks: Vec<(PathBuf, Mark)>,
    /// The patterns the scan used, to tell a change of the settings from a rescan.
    pub keywords: Vec<String>,
    pub case_sensitive: bool,
}

impl Global for TodoData {}

/// One mark in a text: the line from zero, the column in characters, the pattern and what
/// follows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    pub column: usize,
    pub keyword: String,
    pub text: String,
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn same_case(left: char, right: char) -> bool {
    left.to_lowercase().eq(right.to_lowercase())
}

fn starts_with_case(text: &str, pattern: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        return text.starts_with(pattern);
    }
    let mut text = text.chars();
    pattern
        .chars()
        .all(|wanted| text.next().is_some_and(|found| same_case(found, wanted)))
}

fn find_pattern(text: &str, pattern: &str, case_sensitive: bool) -> Option<(usize, usize)> {
    if pattern.is_empty() {
        return None;
    }
    for (start, _) in text.char_indices() {
        if !starts_with_case(&text[start..], pattern, case_sensitive) {
            continue;
        }
        let end = text[start..]
            .char_indices()
            .nth(pattern.chars().count())
            .map_or(text.len(), |(offset, _)| start + offset);
        return Some((start, end));
    }
    None
}

fn mark_in_line(content: &str, pattern: &str, case_sensitive: bool) -> Option<(usize, usize)> {
    let mut from = 0;
    while from < content.len() {
        let (at, end) = find_pattern(&content[from..], pattern, case_sensitive)?;
        let start = from + at;
        let before = content[..start].chars().next_back();
        let after = content[end..].chars().next();
        if !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char) {
            return Some((start, end));
        }
        from = end;
    }
    None
}

/// Finds the first pattern of every line that stands as a word of its own.
pub fn find_marks(text: &str, keywords: &[String], case_sensitive: bool) -> Vec<Mark> {
    let mut marks = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let found = keywords.iter().filter_map(|keyword| {
            let (start, end) = mark_in_line(content, keyword, case_sensitive)?;
            Some((start, keyword, end))
        });
        if let Some((start, keyword, end)) = found.min_by_key(|(start, ..)| *start) {
            let rest = content[end..].trim_start_matches([':', ' ', '\t', '-']);
            marks.push(Mark {
                line,
                column: content[..start].chars().count(),
                keyword: keyword.clone(),
                text: rest.trim().to_string(),
            });
        }
    }
    marks
}

pub(crate) fn skipped(name: &str, more: &[String]) -> bool {
    let hidden = name.starts_with('.') && name.len() > 1 && name != ".github";
    SKIPPED.contains(&name) || hidden || more.iter().any(|folder| folder == name)
}

fn key_of(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn is_under_any(path: &Path, folders: &[PathBuf]) -> bool {
    let path = key_of(path);
    folders.iter().any(|folder| {
        let folder = key_of(folder);
        path == folder || path.starts_with(&format!("{folder}/"))
    })
}

/// Reads the text files under every root, skipping the folders the project excludes, and
/// lists the marks. Blocks: not for the UI thread.
pub fn scan_roots(
    roots: &[PathBuf],
    more_skipped: &[String],
    excluded: &[PathBuf],
    keywords: &[String],
    case_sensitive: bool,
) -> Vec<(PathBuf, Mark)> {
    // A module inside the project is read with the project; a root alone in a folder of
    // its own is read by itself.
    let mut roots: Vec<PathBuf> = roots.iter().filter(|root| root.is_dir()).cloned().collect();
    roots.sort();
    roots.dedup();
    let keys: Vec<String> = roots.iter().map(|root| key_of(root)).collect();
    roots.retain(|root| {
        let key = key_of(root);
        !keys
            .iter()
            .any(|other| *other != key && key.starts_with(&format!("{other}/")))
    });
    let mut found = Vec::new();
    for root in &roots {
        let mut folders = vec![root.clone()];
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
                let path = entry.path();
                if kind.is_dir() {
                    if !skipped(&name, more_skipped) && !is_under_any(&path, excluded) {
                        folders.push(path);
                    }
                    continue;
                }
                if kind.is_file() && !is_under_any(&path, excluded) {
                    let small = entry
                        .metadata()
                        .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
                    if !small {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    if bytes.contains(&0) {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&bytes);
                    for mark in find_marks(&text, keywords, case_sensitive) {
                        found.push((path.clone(), mark));
                    }
                }
            }
        }
    }
    found.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    found
}

/// [`scan_roots`] for one folder with the patterns of a project that never opened the
/// Settings window.
#[cfg(test)]
pub fn scan(root: &Path, more_skipped: &[String]) -> Vec<(PathBuf, Mark)> {
    let keywords: Vec<String> = DEFAULT_KEYWORDS.iter().map(|it| (*it).to_owned()).collect();
    scan_roots(&[root.to_path_buf()], more_skipped, &[], &keywords, true)
}

fn keywords_of(settings: &Settings) -> Vec<String> {
    settings
        .todo_templates
        .iter()
        .map(|template| template.trim().to_owned())
        .filter(|template| !template.is_empty())
        .collect()
}

#[cfg(test)]
fn items_of(root: &Path, marks: Vec<(PathBuf, Mark)>) -> Vec<Item> {
    marks
        .into_iter()
        .map(|(path, mark)| item_of(root, &path, &mark, MATCH_ICON))
        .collect()
}

/// The icon of the file, as the Project tree and the tabs give it.
fn file_icon(panel: &ItemsPanel, path: &Path, cx: &App) -> &'static str {
    panel
        .workspace()
        .read_with(cx, |workspace, _| workspace.icons().icon_for(path))
        .unwrap_or(FILE_ICON)
}

pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<ItemsPanel> {
    let panel = ItemsPanel::create("TODO", TODO_TOOL, "No TODO items found", workspace, cx);
    panel.update(cx, |_, cx| {
        cx.observe_global::<Settings>(settings_changed).detach();
        // The modules of the project are known once the run panel has read the model, and
        // a module may sit outside the root of the window.
        if let Some(runs) = cx.try_global::<ActiveRunPanel>().map(|runs| runs.0.clone()) {
            cx.subscribe(&runs, |panel, _, _: &crate::run::ConfigsChanged, cx| {
                rescan_project(panel, cx);
            })
            .detach();
            cx.subscribe(
                &runs,
                |panel, _, _: &crate::run::ProjectModelChanged, cx| {
                    rescan_project(panel, cx);
                },
            )
            .detach();
        }
    });
    rescan(&panel, root, cx);
    panel
}

/// Reads the project again with the roots of a model that has just arrived.
fn rescan_project(panel: &mut ItemsPanel, cx: &mut Context<ItemsPanel>) {
    let root = panel
        .workspace()
        .read_with(cx, |workspace, _| workspace.root().to_path_buf())
        .ok();
    if let Some(root) = root {
        let panel = cx.entity();
        rescan(&panel, root, cx);
    }
}

/// A change of the settings is a rescan when the patterns moved, and a new grouping of the
/// same marks otherwise.
fn settings_changed(panel: &mut ItemsPanel, cx: &mut Context<ItemsPanel>) {
    let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
    let keywords = keywords_of(&settings);
    let case_sensitive = settings.todo_case_sensitive;
    let stale = cx
        .try_global::<TodoData>()
        .is_none_or(|data| data.keywords != keywords || data.case_sensitive != case_sensitive);
    if !stale {
        regroup(panel, cx);
        return;
    }
    let root = cx
        .try_global::<TodoData>()
        .map(|data| data.root.clone())
        .or_else(|| {
            panel
                .workspace()
                .read_with(cx, |workspace, _| workspace.root().to_path_buf())
                .ok()
        });
    if let Some(root) = root {
        let panel = cx.entity();
        rescan(&panel, root, cx);
    }
}

/// The roots the scan reads: the window root and the folders of the modules the build tool
/// named, which may lie outside it, with the folders the model excludes.
fn roots_of(root: &Path, cx: &App) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut roots = vec![root.to_path_buf()];
    let mut excluded = Vec::new();
    if let Some(panel) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
        && let Some(model) = panel.read(cx).model()
    {
        if !model.root.as_os_str().is_empty() {
            roots.push(model.root.clone());
        }
        for module in &model.modules {
            roots.push(module.dir.clone());
            excluded.extend(module.excluded_roots.iter().cloned());
            excluded.extend(module.generated_roots.iter().cloned());
        }
    }
    (roots, excluded)
}

/// Reads the project again in the background and fills the list.
pub fn rescan(panel: &Entity<ItemsPanel>, root: PathBuf, cx: &mut App) {
    let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
    let keywords = keywords_of(&settings);
    let case_sensitive = settings.todo_case_sensitive;
    let more = cx
        .try_global::<SkippedFolders>()
        .map_or_else(Vec::new, |skipped| skipped.0.clone());
    let (roots, excluded) = roots_of(&root, cx);
    let scan_keywords = keywords.clone();
    let task = cx
        .background_executor()
        .spawn(async move { scan_roots(&roots, &more, &excluded, &scan_keywords, case_sensitive) });
    let panel = panel.downgrade();
    cx.spawn(async move |cx| {
        let marks = task.await;
        cx.update(|cx| {
            cx.set_global(TodoData {
                root: root.clone(),
                marks,
                keywords,
                case_sensitive,
            });
        });
        panel.update(cx, regroup).ok();
    })
    .detach();
}

fn same_path(left: &Path, right: &Path) -> bool {
    key_of(left) == key_of(right)
}

/// The changed files the Commit window lists, as absolute paths; `None` when the project
/// is not in a repository.
fn changed_paths(cx: &App) -> Option<Vec<PathBuf>> {
    let git = cx.try_global::<ActiveGit>()?.0.clone();
    let git = git.read(cx);
    let root = git.repo()?.root().to_path_buf();
    Some(
        git.status()
            .files
            .iter()
            .map(|file| root.join(&file.path))
            .collect(),
    )
}

fn set_filter(cx: &mut App, change: impl FnOnce(&mut TodoFilter)) {
    let mut filter = cx.try_global::<TodoFilter>().cloned().unwrap_or_default();
    change(&mut filter);
    cx.set_global(filter);
    let panel = cx.try_global::<ActiveTodo>().map(|active| active.0.clone());
    if let Some(panel) = panel {
        // The click comes from the panel itself, so the list is built again once the
        // window is free.
        cx.defer(move |cx| {
            panel.update(cx, regroup);
        });
    }
}

/// Rebuilds the list with the grouping rules of the settings and the filter of the window.
pub fn regroup(panel: &mut ItemsPanel, cx: &mut Context<ItemsPanel>) {
    let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
    let filter = cx.try_global::<TodoFilter>().cloned().unwrap_or_default();
    let current = panel
        .workspace()
        .read_with(cx, |workspace, _| workspace.active_path())
        .ok()
        .flatten();
    let changed = changed_paths(cx);
    // Without a repository the Changed Files scope has nothing to show, so the list goes
    // back to the whole project.
    let scope = if filter.scope == TodoScope::ChangedFiles && changed.is_none() {
        let mut reset = filter.clone();
        reset.scope = TodoScope::Project;
        cx.set_global(reset);
        TodoScope::Project
    } else {
        filter.scope
    };
    let Some(data) = cx.try_global::<TodoData>() else {
        return;
    };
    let root = data.root.clone();
    let template = filter
        .template
        .clone()
        .filter(|name| data.keywords.contains(name));
    let mut entries: Vec<(Vec<(String, &'static str)>, Item)> = data
        .marks
        .iter()
        .filter(|(path, mark)| {
            if template.as_deref().is_some_and(|name| mark.keyword != name) {
                return false;
            }
            match scope {
                TodoScope::Project => true,
                TodoScope::CurrentFile => current
                    .as_deref()
                    .is_some_and(|current| same_path(current, path)),
                TodoScope::ChangedFiles => changed
                    .as_ref()
                    .is_some_and(|files| files.iter().any(|file| same_path(file, path))),
            }
        })
        .map(|(path, mark)| {
            let item = item_of(&root, path, mark, MATCH_ICON);
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
    let mut previous_file: Option<PathBuf> = None;
    for (keys, item) in entries {
        let common = previous
            .iter()
            .zip(&keys)
            .take_while(|(left, right)| left.as_str() == right.0.as_str())
            .count();
        if common < previous.len() {
            previous_file = None;
        }
        for (depth, key) in keys.iter().enumerate().skip(common) {
            rows.push(PanelRow::Group {
                label: key.0.clone(),
                depth,
                icon: key.1,
            });
        }
        previous = keys.iter().map(|(key, _)| key.clone()).collect();
        // A file under the groups keeps its matches together, as the tree of IDEA does.
        if previous_file.as_deref() != Some(item.path.as_path()) {
            let name = item.path.file_name().map_or_else(
                || item.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let icon = file_icon(panel, &item.path, cx);
            rows.push(PanelRow::Group {
                label: name,
                depth: previous.len(),
                icon,
            });
            previous_file = Some(item.path.clone());
        }
        let index = items.len();
        rows.push(PanelRow::Item {
            index,
            depth: previous.len() + 1,
            excluded: false,
        });
        items.push(item);
    }
    let count = items.len();
    panel.set_title(format!("TODO ({count})"), cx);
    let buttons = header_buttons(&settings, panel, &filter);
    panel.set_header_buttons(buttons, cx);
    let tabs = scope_tabs(scope, changed.is_some());
    panel.set_tabs(tabs, cx);
    panel.set_view(items, rows, cx);
}

/// The scope of the list as the tabs above it: Changed Files is offered only when the
/// project is in a repository and its status is known.
fn scope_tabs(scope: TodoScope, changed: bool) -> Vec<PanelTab> {
    [
        (TodoScope::Project, "todo-scope-project"),
        (TodoScope::CurrentFile, "todo-scope-current-file"),
        (TodoScope::ChangedFiles, "todo-scope-changed-files"),
    ]
    .into_iter()
    .filter(|(place, _)| *place != TodoScope::ChangedFiles || changed)
    .map(|(place, id)| PanelTab {
        id,
        label: place.label().to_owned(),
        active: scope == place,
        run: Rc::new(move |_, cx| {
            set_filter(cx, |filter| filter.scope = place);
        }),
    })
    .collect()
}

fn group_keys(
    path: &Path,
    root: &Path,
    settings: &Settings,
    cx: &App,
) -> Vec<(String, &'static str)> {
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
        keys.extend(
            split_name(&module)
                .into_iter()
                .enumerate()
                .map(|(depth, name)| (name, if depth == 0 { MODULE_ICON } else { FOLDER_ICON })),
        );
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
            keys.push((package, PACKAGE_ICON));
        } else {
            keys.extend(
                split_name(&package)
                    .into_iter()
                    .enumerate()
                    .map(|(depth, name)| {
                        (
                            name,
                            if depth == 0 {
                                PACKAGE_ICON
                            } else {
                                FOLDER_ICON
                            },
                        )
                    }),
            );
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

fn header_buttons(
    settings: &Settings,
    panel_ref: &ItemsPanel,
    filter: &TodoFilter,
) -> Vec<HeaderButton> {
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
                run: Rc::new(move |_, cx| {
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
    let mut buttons = vec![
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
    ];
    let templates = keywords_of(settings);
    let current = filter
        .template
        .clone()
        .filter(|name| templates.contains(name));
    let label = current.clone().unwrap_or_else(|| "All".to_owned());
    buttons.push(HeaderButton {
        id: "todo-filter-template",
        label,
        active: current.is_some(),
        icon: None,
        tooltip: Some("Filter by Template".to_owned()),
        shortcut: None,
        run: Rc::new(move |_, cx| {
            let next = match &current {
                None => templates.first().cloned(),
                Some(name) => templates
                    .iter()
                    .position(|known| known == name)
                    .and_then(|index| templates.get(index + 1))
                    .cloned(),
            };
            set_filter(cx, |filter| filter.template = next);
        }),
    });
    buttons
}

fn item_of(root: &Path, path: &Path, mark: &Mark, icon: &'static str) -> Item {
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
        icon,
    }
}

#[cfg(test)]
mod tests {
    use super::{Mark, find_marks, items_of, scan, scan_roots};
    use std::path::{Path, PathBuf};

    #[test]
    fn marks_are_whole_words_with_their_text() {
        let text = "int a; // TODO: fix this\nTODOS are not marks\n  // FIXME - later\nx = METODO;";
        let keywords = ["TODO".to_string(), "FIXME".to_string()];
        let marks = find_marks(text, &keywords, true);
        assert_eq!(
            marks,
            [
                Mark {
                    line: 0,
                    column: 10,
                    keyword: "TODO".into(),
                    text: "fix this".into()
                },
                Mark {
                    line: 2,
                    column: 5,
                    keyword: "FIXME".into(),
                    text: "later".into()
                },
            ]
        );
    }

    #[test]
    fn a_bare_mark_has_no_text_and_the_column_counts_characters() {
        let keywords = ["TODO".to_string()];
        let marks = find_marks("é TODO", &keywords, true);
        assert_eq!(marks[0].column, 2);
        assert!(marks[0].text.is_empty());
    }

    #[test]
    fn a_custom_pattern_is_found_and_case_follows_the_setting() {
        let keywords = ["hack".to_string()];
        assert_eq!(find_marks("// HACK", &keywords, true).len(), 0);
        assert_eq!(find_marks("// HACK", &keywords, false).len(), 1);
        assert_eq!(
            find_marks("// HACK: soon", &["HACK".to_string()], true)[0].text,
            "soon"
        );
    }

    #[test]
    fn the_scan_reads_every_root_and_skips_the_excluded_folders() {
        let base = test_support::scratch("todo-roots");
        let project = base.join("project");
        let module = base.join("module");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(module.join("src")).unwrap();
        std::fs::create_dir_all(module.join("generated")).unwrap();
        std::fs::write(project.join("src/A.txt"), "// TODO from the project\n").unwrap();
        std::fs::write(module.join("src/B.txt"), "// TODO from the module\n").unwrap();
        std::fs::write(module.join("generated/C.txt"), "// TODO generated\n").unwrap();

        let found = scan_roots(
            &[project, module.clone()],
            &[],
            &[module.join("generated")],
            &["TODO".to_string()],
            true,
        );

        let mut labels: Vec<String> = found.iter().map(|(_, mark)| mark.text.clone()).collect();
        labels.sort();
        assert_eq!(labels, ["from the module", "from the project"]);
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
