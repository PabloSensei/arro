//! The usages of a symbol: Alt+F7 lists them by file in the tool window, Ctrl+Alt+F7
//! shows the same places in a popup at the caret.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use editor_view::{DetailedUsage, EditorView, Semantic, Spot, Usage, UsagesUpdate, uri_path};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt;
use lang_api::{
    DocumentSpot, FileSymbol, FileSymbolsQuery, HierarchyDirection, HierarchyQuery,
    LanguageSupport, MethodHierarchyQuery, MethodRelation, NavigationKind, NavigationQuery,
    SymbolKind, TextPoint, UsageEntry, UsageKind, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, USAGES_TOOL, Workspace};

use crate::items::{HeaderButton, Item, ItemsPanel, PanelRow};
use crate::navigation;
use crate::outline::OpenEditors;
use crate::project::ActiveProjectPanel;

const POPUP_CONTEXT: &str = "ShowUsagesPopup";
const POPUP_WIDTH: f32 = 640.;
const POPUP_HEIGHT: f32 = 420.;
const SNIPPET: usize = 120;
const FILE_WIDTH: f32 = 190.;

/// Files read by the background tasks of the popup, for the window tests.
#[cfg(test)]
static BACKGROUND_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn background_reads() -> usize {
    BACKGROUND_READS.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn reset_background_reads() {
    BACKGROUND_READS.store(0, std::sync::atomic::Ordering::SeqCst);
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

actions!(show_usages_popup, [Dismiss]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(POPUP_CONTEXT))]);
}

/// One tab of the Usages window: the places of one symbol search.
struct UsagesTab {
    /// Bumped on every search, so a late answer of an old search is dropped.
    serial: u64,
    /// The symbol name, the label of the tab.
    name: String,
    /// What the title of the window says was searched.
    prefix: String,
    usages: Vec<DetailedUsage>,
    excluded: Vec<bool>,
    /// The options of the search; `None` for a search that used the defaults.
    options: Option<FindUsagesOptions>,
    pinned: bool,
}

/// Keeps the latest usages and the window that lists them.
pub struct UsagesHub {
    root: PathBuf,
    panel: Entity<ItemsPanel>,
    workspace: WeakEntity<Workspace>,
    /// The tabs of the window, oldest first.
    tabs: Vec<UsagesTab>,
    active: usize,
    /// The number of the next search.
    serial: u64,
    /// The usage of each row of the list, in the order the rows are drawn.
    sources: Vec<usize>,
}

/// The hub of the window, for the editor tab that receives the answer.
pub struct ActiveUsages(pub Entity<UsagesHub>);

impl Global for ActiveUsages {}

/// The popup of Show Usages that is on screen, if any.
pub struct ActiveShowUsages(pub Option<Entity<ShowUsagesPopup>>);

impl Global for ActiveShowUsages {}

/// Where Find Usages looks, the scope of Ctrl+Alt+Shift+F7.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FindUsagesScope {
    #[default]
    ProjectFiles,
    ProjectAndLibraries,
    Module,
    CurrentFile,
    OpenFiles,
    Selected,
}

impl FindUsagesScope {
    pub const ALL: [FindUsagesScope; 6] = [
        FindUsagesScope::ProjectFiles,
        FindUsagesScope::ProjectAndLibraries,
        FindUsagesScope::Module,
        FindUsagesScope::CurrentFile,
        FindUsagesScope::OpenFiles,
        FindUsagesScope::Selected,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ProjectFiles => "Project Files",
            Self::ProjectAndLibraries => "Project and Libraries",
            Self::Module => "Module",
            Self::CurrentFile => "Current File",
            Self::OpenFiles => "Open Files",
            Self::Selected => "Selected in Tree",
        }
    }
}

/// What kind of symbol the caret is on, which decides the flags of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindUsagesTarget {
    Class,
    Method,
    /// A field, a local variable or a parameter: the flags are readers and writers.
    Field,
}

impl FindUsagesTarget {
    fn of(kind: SymbolKind) -> Self {
        match kind {
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum => Self::Class,
            SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Function => Self::Method,
            _ => Self::Field,
        }
    }
}

/// What the Find Usages dialog applies to the next Alt+F7.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindUsagesOptions {
    pub scope: FindUsagesScope,
    pub usages: bool,
    pub methods: bool,
    pub fields: bool,
    pub overriding: bool,
    pub implementing: bool,
    pub derived: bool,
    pub readers: bool,
    pub writers: bool,
    pub open_in_new_tab: bool,
    /// The kind of symbol the flags were chosen for, once the dialog knows it.
    pub target: Option<FindUsagesTarget>,
}

impl Default for FindUsagesOptions {
    fn default() -> Self {
        Self {
            scope: FindUsagesScope::ProjectFiles,
            usages: true,
            methods: true,
            fields: true,
            overriding: true,
            implementing: true,
            derived: true,
            readers: true,
            writers: true,
            open_in_new_tab: false,
            target: None,
        }
    }
}

impl FindUsagesOptions {
    /// Whether the uses of the symbol itself pass the Usages flag. A field has no such
    /// flag: its readers and writers are the whole search.
    fn shows_itself(&self) -> bool {
        self.usages || self.target == Some(FindUsagesTarget::Field)
    }

    /// Whether the kind of the use passes the readers and writers flags. Only a field
    /// and a symbol of an unknown kind have those flags in the dialog.
    fn shows_kind(&self, kind: UsageKind) -> bool {
        if matches!(
            self.target,
            Some(FindUsagesTarget::Class | FindUsagesTarget::Method)
        ) {
            return true;
        }
        match kind {
            UsageKind::Read => self.readers,
            UsageKind::Write => self.writers,
            _ => true,
        }
    }
}

/// The options of the next Alt+F7. The dialog keeps them after a search, the way IDEA
/// remembers the flags of Find Usages.
pub struct PendingFindUsages(pub Option<FindUsagesOptions>);

impl Global for PendingFindUsages {}

/// The Find Usages dialog that is on screen, if any.
pub struct ActiveFindUsagesSettings(#[allow(dead_code)] pub Option<Entity<FindUsagesSettings>>);

impl Global for ActiveFindUsagesSettings {}

impl UsagesHub {
    pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let panel = ItemsPanel::create(
            "Usages",
            USAGES_TOOL,
            "No usages found.",
            workspace.clone(),
            cx,
        );
        let hub = cx.new(|cx| {
            cx.observe_global::<Settings>(|this: &mut Self, cx| this.regroup(cx))
                .detach();
            Self {
                root,
                panel: panel.clone(),
                workspace,
                tabs: Vec::new(),
                active: 0,
                serial: 0,
                sources: Vec::new(),
            }
        });
        let weak = hub.downgrade();
        panel.update(cx, |panel, _| {
            panel.set_toggle_handler(std::rc::Rc::new(move |index, excluded, cx| {
                weak.update(cx, |hub, _| {
                    if let Some(source) = hub.sources.get(index).copied()
                        && let Some(tab) = hub.tabs.get_mut(hub.active)
                        && let Some(entry) = tab.excluded.get_mut(source)
                    {
                        *entry = excluded;
                    }
                })
                .ok();
            }));
        });
        hub
    }

    pub fn panel(&self) -> &Entity<ItemsPanel> {
        &self.panel
    }

    fn tab(&self) -> Option<&UsagesTab> {
        self.tabs.get(self.active)
    }

    /// Replaces the list with the uses of one symbol, grouped by file.
    pub fn set_usages(&mut self, usages: Vec<Usage>, cx: &mut Context<Self>) {
        let detailed = usages
            .into_iter()
            .map(|usage| DetailedUsage {
                name: usage.name,
                target: usage.target,
                kind: UsageKind::Unknown,
                container: None,
                module: None,
            })
            .collect();
        self.set_detailed_usages(detailed, cx);
    }

    /// Replaces the list with the uses of one symbol and the grouping the settings ask.
    pub fn set_detailed_usages(&mut self, mut usages: Vec<DetailedUsage>, cx: &mut Context<Self>) {
        let options = cx
            .try_global::<PendingFindUsages>()
            .and_then(|pending| pending.0.clone());
        let name = usages
            .first()
            .map(|usage| usage.name.clone())
            .unwrap_or_default();
        let mut prefix = "Usages".to_owned();
        if let Some(options) = &options {
            usages.retain(|usage| {
                options.shows_itself()
                    && self.in_scope(usage, options, cx)
                    && options.shows_kind(usage.kind)
            });
            prefix = search_title(&name, options);
        }
        let new_tab = self.tabs.is_empty()
            || options
                .as_ref()
                .is_some_and(|options| options.open_in_new_tab)
            || self.tab().is_some_and(|tab| tab.pinned);
        self.serial += 1;
        let serial = self.serial;
        let tab = UsagesTab {
            serial,
            name: name.clone(),
            prefix,
            excluded: vec![false; usages.len()],
            usages,
            options: options.clone(),
            pinned: false,
        };
        if new_tab {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        } else {
            self.tabs[self.active] = tab;
        }
        self.regroup(cx);
        if options.is_some() {
            self.load_extra(serial, cx);
        }
    }

    /// Asks the language for the member and hierarchy searches of the options, then
    /// adds what it finds to the tab of `serial`.
    fn load_extra(&mut self, serial: u64, cx: &mut Context<Self>) {
        let Some(options) = self.tab().and_then(|tab| tab.options.clone()) else {
            return;
        };
        let wants_members = options.methods || options.fields || options.derived;
        let wants_hierarchy = options.overriding || options.implementing;
        if !wants_members && !wants_hierarchy {
            return;
        }
        let Some(support) = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone())
        else {
            return;
        };
        let Some(spot) = self.current_spot(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let Some(target) = target_of_symbol(&*support, &spot).await else {
                return;
            };
            // A field or a variable has no members or hierarchy to add.
            if target == FindUsagesTarget::Field {
                return;
            }
            let Some(symbol) = declaration_symbol(&*support, &spot).await else {
                return;
            };
            let symbol_name = symbol.name.clone();
            let mut extra = Vec::new();
            if target == FindUsagesTarget::Class {
                if options.methods || options.fields {
                    for member in &symbol.children {
                        let wanted = match FindUsagesTarget::of(member.kind) {
                            FindUsagesTarget::Method => options.methods,
                            FindUsagesTarget::Field => options.fields,
                            FindUsagesTarget::Class => false,
                        };
                        if !wanted {
                            continue;
                        }
                        let member_spot = DocumentSpot {
                            uri: spot.uri.clone(),
                            version: spot.version,
                            point: member.name_range.start,
                        };
                        extra.extend(
                            usages_of_spot(
                                &*support,
                                member_spot,
                                member.name.clone(),
                                &background,
                            )
                            .await,
                        );
                    }
                    // The members answer before the hierarchy does; show it at once.
                    if !extra.is_empty() {
                        let batch = std::mem::take(&mut extra);
                        let _ = this.update(cx, |hub, cx| {
                            hub.merge_extra(serial, target, symbol_name.clone(), batch, cx)
                        });
                    }
                }
                if options.derived
                    && let Some(provider) = support.hierarchy()
                {
                    let mut replies = ask(
                        provider,
                        HierarchyQuery {
                            spot: spot.clone(),
                            direction: HierarchyDirection::Subtypes,
                        },
                    );
                    if let Some(answer) = replies.next().await {
                        for entry in answer.value {
                            let Some(place) = entry.place else {
                                continue;
                            };
                            let name = entry.name;
                            let nested = DocumentSpot {
                                uri: place.uri,
                                version: spot.version,
                                point: place.span.start,
                            };
                            extra
                                .extend(usages_of_spot(&*support, nested, name, &background).await);
                        }
                    }
                }
            } else if target == FindUsagesTarget::Method {
                if options.overriding
                    && let Some(provider) = support.method_hierarchy()
                {
                    let mut replies = ask(provider, MethodHierarchyQuery(spot.clone()));
                    if let Some(answer) = replies.next().await {
                        for entry in answer.value {
                            if entry.relation != MethodRelation::Defines {
                                continue;
                            }
                            let Some(place) = entry.place else {
                                continue;
                            };
                            if place.uri == spot.uri && place.span.start == spot.point {
                                continue;
                            }
                            let nested = DocumentSpot {
                                uri: place.uri,
                                version: spot.version,
                                point: place.span.start,
                            };
                            extra.extend(
                                usages_of_spot(&*support, nested, entry.method, &background).await,
                            );
                        }
                    }
                }
                if options.implementing
                    && let Some(provider) = support.navigation()
                {
                    let mut replies = ask(
                        provider,
                        NavigationQuery {
                            kind: NavigationKind::Implementation,
                            spot: spot.clone(),
                        },
                    );
                    if let Some(answer) = replies.next().await {
                        for place in answer.value {
                            if place.uri == spot.uri && place.span.start == spot.point {
                                continue;
                            }
                            let nested = DocumentSpot {
                                uri: place.uri,
                                version: spot.version,
                                point: place.span.start,
                            };
                            extra.extend(
                                usages_of_spot(&*support, nested, symbol.name.clone(), &background)
                                    .await,
                            );
                        }
                    }
                }
            }
            let _ = this.update(cx, |hub, cx| {
                hub.merge_extra(serial, target, symbol_name, extra, cx)
            });
        })
        .detach();
    }

    /// Adds the answer of a member or hierarchy search to the tab that asked for it.
    fn merge_extra(
        &mut self,
        serial: u64,
        target: FindUsagesTarget,
        symbol_name: String,
        extra: Vec<DetailedUsage>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.serial == serial) else {
            return;
        };
        let options = self.tabs[index].options.clone().unwrap_or_default();
        let mut known: HashSet<(String, u32, u32, u32, u32)> =
            self.tabs[index].usages.iter().map(usage_key).collect();
        for usage in extra {
            // The member and hierarchy flags already chose these places; the Usages
            // flag applies to the uses of the symbol itself, not to its members.
            if !self.in_scope(&usage, &options, cx) || !options.shows_kind(usage.kind) {
                continue;
            }
            if known.insert(usage_key(&usage)) {
                self.tabs[index].usages.push(usage);
            }
        }
        self.tabs[index].excluded = vec![false; self.tabs[index].usages.len()];
        if self.tabs[index].name.is_empty() {
            self.tabs[index].name = symbol_name.clone();
        }
        self.tabs[index].prefix = tab_title(&symbol_name, &options, target);
        if index == self.active {
            self.regroup(cx);
        }
    }

    /// The place of the caret in the active editor.
    fn current_spot(&self, cx: &App) -> Option<DocumentSpot> {
        let workspace = self.workspace.upgrade()?;
        let (_, view) = workspace.read(cx).active_editor()?;
        let view = view.downcast::<EditorView>().ok()?;
        view.read(cx).spot().map(|(spot, _)| spot)
    }

    /// Opens the tab at `index`.
    pub fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.active = index;
        self.regroup(cx);
    }

    /// Closes the tab at `index`.
    pub fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if index < self.active {
            self.active -= 1;
        }
        self.regroup(cx);
    }

    /// Pins or unpins the tab at `index`.
    pub fn set_pinned(&mut self, index: usize, pinned: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.pinned = pinned;
            cx.notify();
        }
    }

    /// The symbols of the tabs, for the tests.
    #[cfg(test)]
    pub(crate) fn tab_names(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.name.clone()).collect()
    }

    /// The active tab, for the tests.
    #[cfg(test)]
    pub(crate) fn active_tab(&self) -> usize {
        self.active
    }

    fn in_scope(&self, usage: &DetailedUsage, options: &FindUsagesOptions, cx: &App) -> bool {
        let Some(path) = uri_path(&usage.target.uri) else {
            return false;
        };
        match options.scope {
            FindUsagesScope::ProjectFiles => path.starts_with(&self.root),
            FindUsagesScope::ProjectAndLibraries => true,
            FindUsagesScope::Module => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return true;
                };
                let Some(current) = workspace.read(cx).active_path() else {
                    return true;
                };
                let module = self.module_of(&current, cx);
                module.is_some() && module == self.module_of(&path, cx)
            }
            FindUsagesScope::CurrentFile => self
                .workspace
                .upgrade()
                .and_then(|workspace| workspace.read(cx).active_path())
                .is_some_and(|current| current == path),
            FindUsagesScope::OpenFiles => self.workspace.upgrade().is_some_and(|workspace| {
                workspace
                    .read(cx)
                    .open_tabs()
                    .into_iter()
                    .flatten()
                    .any(|open| open == path)
            }),
            FindUsagesScope::Selected => {
                cx.try_global::<ActiveProjectPanel>().is_some_and(|panel| {
                    panel
                        .0
                        .read(cx)
                        .selected_paths()
                        .iter()
                        .any(|selected| path.starts_with(selected))
                })
            }
        }
    }

    fn module_of(&self, path: &Path, cx: &App) -> Option<String> {
        cx.try_global::<Semantic>()
            .and_then(|semantic| semantic.0.module_of(path))
            .or_else(|| {
                let relative = path.strip_prefix(&self.root).unwrap_or(path);
                relative
                    .parent()
                    .and_then(|parent| parent.components().next())
                    .map(|first| first.as_os_str().to_string_lossy().into_owned())
                    .or_else(|| {
                        self.root
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
            })
    }

    fn item(&self, usage: &DetailedUsage) -> Option<Item> {
        let path = uri_path(&usage.target.uri)?;
        let relative = path.strip_prefix(&self.root).unwrap_or(&path);
        let place = format!(
            "{}:{}:{}",
            relative.display(),
            usage.target.span.start.line + 1,
            usage.target.span.start.character + 1
        );
        let name = if usage.name.is_empty() {
            "usage"
        } else {
            usage.name.as_str()
        };
        Some(Item {
            path,
            line: usage.target.span.start.line as usize,
            column: usage.target.span.start.character as usize,
            label: name.to_owned(),
            hint: place.replace('\\', "/"),
            icon: "codeInsight/search",
        })
    }

    /// Builds the rows from the usages, the filters and the grouping of the settings.
    pub fn regroup(&mut self, cx: &mut Context<Self>) {
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let Some(tab) = self.tabs.get(self.active) else {
            self.sources.clear();
            let buttons = header_buttons(&settings, cx);
            self.panel.update(cx, |panel, cx| {
                panel.set_header_buttons(buttons, cx);
                panel.set_view(Vec::new(), Vec::new(), cx);
                panel.set_counted_title("Usages", cx);
            });
            return;
        };
        let mut rows: Vec<(Vec<String>, usize, Item)> = Vec::new();
        for (index, usage) in tab.usages.iter().enumerate() {
            if !self.shown(usage, &settings) {
                continue;
            }
            let Some(item) = self.item(usage) else {
                continue;
            };
            let keys = self.group_keys(usage, &settings, cx);
            rows.push((keys, index, item));
        }
        let excluded = tab.excluded.clone();
        let prefix = tab.prefix.clone();
        rows.sort_by(|a, b| {
            (&a.0, &a.2.path, a.2.line, a.2.column).cmp(&(&b.0, &b.2.path, b.2.line, b.2.column))
        });
        let mut items = Vec::with_capacity(rows.len());
        let mut panel_rows = Vec::new();
        let mut sources = Vec::with_capacity(rows.len());
        let mut previous: Vec<String> = Vec::new();
        for (keys, index, item) in rows {
            let common = previous
                .iter()
                .zip(&keys)
                .take_while(|(left, right)| left == right)
                .count();
            for (depth, key) in keys.iter().enumerate().skip(common) {
                panel_rows.push(PanelRow::Group {
                    label: key.clone(),
                    depth,
                    icon: "general/folder",
                });
            }
            previous = keys;
            let at = items.len();
            panel_rows.push(PanelRow::Item {
                index: at,
                depth: previous.len(),
                excluded: excluded.get(index).copied().unwrap_or(false),
            });
            items.push(item);
            sources.push(index);
        }
        self.sources = sources;
        let buttons = header_buttons(&settings, cx);
        self.panel.update(cx, |panel, cx| {
            panel.set_header_buttons(buttons, cx);
            panel.set_view(items, panel_rows, cx);
            panel.set_counted_title(&prefix, cx);
        });
    }

    fn shown(&self, usage: &DetailedUsage, settings: &Settings) -> bool {
        match usage.kind {
            UsageKind::Read => settings.usage_show_read,
            UsageKind::Write => settings.usage_show_write,
            UsageKind::Import => settings.usage_show_imports,
            _ => true,
        }
    }

    fn group_keys(&self, usage: &DetailedUsage, settings: &Settings, cx: &App) -> Vec<String> {
        let path = uri_path(&usage.target.uri).unwrap_or_default();
        let relative = path.strip_prefix(&self.root).unwrap_or(&path);
        let mut keys = Vec::new();
        if settings.usage_group_usage_type {
            keys.push(usage.kind.label().to_owned());
        }
        if settings.usage_group_module {
            let module = usage
                .module
                .clone()
                .or_else(|| self.module_of(&path, cx))
                .unwrap_or_default();
            if settings.usage_flatten_modules {
                keys.push(module);
            } else {
                keys.extend(split_name(&module));
            }
        }
        if settings.usage_group_directory {
            let folder = relative.parent().unwrap_or(Path::new(""));
            keys.push(display_path(folder));
        }
        if settings.usage_group_directory_structure {
            let folder = relative.parent().unwrap_or(Path::new(""));
            keys.extend(
                folder
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned()),
            );
        }
        if settings.usage_group_file_structure {
            keys.push(display_path(relative));
            let container = usage
                .container
                .clone()
                .filter(|container| !container.is_empty())
                .or_else(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            keys.extend(split_name(&container));
        }
        keys
    }

    /// One of the grouping or filtering rules of the header.
    pub fn change_setting(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.update_settings(change, cx))
            .ok();
    }

    /// The tree of the list as plain text, for Export to Text File.
    pub fn export_text(&self, cx: &App) -> String {
        self.panel.read(cx).export_text()
    }

    /// The group headers, for the tests.
    #[cfg(test)]
    pub(crate) fn group_labels(&self, cx: &App) -> Vec<String> {
        self.panel
            .read(cx)
            .rows()
            .iter()
            .filter_map(|row| match row {
                PanelRow::Group { label, .. } => Some(label.clone()),
                PanelRow::Item { .. } => None,
            })
            .collect()
    }

    /// The items the list shows and the ones it excludes, for the tests.
    #[cfg(test)]
    pub(crate) fn shown_and_excluded(&self, cx: &App) -> (usize, usize) {
        self.panel
            .read(cx)
            .rows()
            .iter()
            .fold((0, 0), |(shown, excluded), row| match row {
                PanelRow::Item { excluded: out, .. } if *out => (shown, excluded + 1),
                PanelRow::Item { .. } => (shown + 1, excluded),
                PanelRow::Group { .. } => (shown, excluded),
            })
    }
    /// The menu of a tab: Pin Tab, Close Tab.
    fn show_tab_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let pinned = tab.pinned;
        let weak = cx.weak_entity();
        let pin = if pinned { "Unpin Tab" } else { "Pin Tab" };
        let pin_weak = weak.clone();
        let close_weak = weak.clone();
        let entries = vec![
            MenuEntry::new(pin, move |_, cx| {
                pin_weak
                    .update(cx, |hub, cx| hub.set_pinned(index, !pinned, cx))
                    .ok();
            }),
            MenuEntry::new("Close Tab", move |_, cx| {
                close_weak
                    .update(cx, |hub, cx| hub.close_tab(index, cx))
                    .ok();
            }),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }
}

impl Render for UsagesHub {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let selected = index == self.active;
                let pinned = tab.pinned;
                let name = tab.name.clone();
                let close = cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.close_tab(index, cx);
                });
                div()
                    .id(("usage-tab", index))
                    .debug_selector(move || format!("usage-tab-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(TAB_RADIUS))
                    .when(selected, |this| this.bg(theme.tab_selected))
                    .when(!selected, |this| this.hover(|style| style.bg(theme.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_tab(index, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.show_tab_menu(index, event.position, cx)
                        }),
                    )
                    .child(if pinned {
                        icon("general/locate", &theme)
                    } else {
                        icon("codeInsight/search", &theme)
                    })
                    .child(div().max_w(px(160.)).truncate().child(name))
                    .child(
                        div()
                            .id(("usage-tab-close", index))
                            .debug_selector(move || format!("usage-tab-close-{index}"))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .rounded(px(3.))
                            .hover(|style| style.bg(theme.selection_hover))
                            .on_click(close)
                            .child(icon("general/closeSmall", &theme)),
                    )
            })
            .collect();
        let strip = (!tabs.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_XS))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .debug_selector(|| "usage-tabs".to_owned())
                .children(tabs)
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .children(strip)
            .child(self.panel.clone())
    }
}

/// A usage that the search of the symbol itself found, before the member and
/// hierarchy searches add the rest.
fn search_title(name: &str, options: &FindUsagesOptions) -> String {
    let name = if name.is_empty() { "symbol" } else { name };
    match options.target {
        Some(FindUsagesTarget::Field) => format!("Usages of fields of {name}"),
        Some(FindUsagesTarget::Method) => format!("Usages of methods of {name}"),
        Some(FindUsagesTarget::Class) if options.methods && options.fields => {
            format!("Usages of methods and fields of {name}")
        }
        Some(FindUsagesTarget::Class) if options.methods => {
            format!("Usages of methods of {name}")
        }
        Some(FindUsagesTarget::Class) if options.fields => {
            format!("Usages of fields of {name}")
        }
        _ if options.methods && options.fields => {
            format!("Usages of methods and fields of {name}")
        }
        _ if options.methods => format!("Usages of methods of {name}"),
        _ if options.fields => format!("Usages of fields of {name}"),
        _ => format!("Usages of {name}"),
    }
}

/// The title once the kind of the symbol is known.
fn tab_title(name: &str, options: &FindUsagesOptions, target: FindUsagesTarget) -> String {
    let name = if name.is_empty() { "symbol" } else { name };
    match target {
        FindUsagesTarget::Class if options.methods && options.fields => {
            format!("Usages of methods and fields of {name}")
        }
        FindUsagesTarget::Class if options.methods => {
            format!("Usages of methods of {name}")
        }
        FindUsagesTarget::Class if options.fields => {
            format!("Usages of fields of {name}")
        }
        FindUsagesTarget::Method if options.overriding || options.implementing => {
            format!("Usages of methods of {name}")
        }
        FindUsagesTarget::Field if options.readers || options.writers => {
            format!("Usages of fields of {name}")
        }
        _ => format!("Usages of {name}"),
    }
}

fn usage_key(usage: &DetailedUsage) -> (String, u32, u32, u32, u32) {
    (
        usage.target.uri.clone(),
        usage.target.span.start.line,
        usage.target.span.start.character,
        usage.target.span.end.line,
        usage.target.span.end.character,
    )
}

fn contains(range: &lang_api::TextSpan, point: TextPoint) -> bool {
    let start = range.start;
    let end = range.end;
    (start.line, start.character) <= (point.line, point.character)
        && (point.line, point.character) <= (end.line, end.character)
}

/// The deepest declaration of the file that contains the point.
fn symbol_at_point(symbols: &[FileSymbol], point: TextPoint) -> Option<&FileSymbol> {
    symbols.iter().find_map(|symbol| {
        if !contains(&symbol.range, point) {
            return None;
        }
        Some(symbol_at_point(&symbol.children, point).unwrap_or(symbol))
    })
}

/// The declaration under the spot, as the language server describes it.
async fn symbol_at(support: &dyn LanguageSupport, spot: &DocumentSpot) -> Option<FileSymbol> {
    let provider = support.file_symbols()?;
    let mut replies = ask(provider, FileSymbolsQuery(spot.uri.clone()));
    let answer = replies.next().await?;
    symbol_at_point(&answer.value, spot.point).cloned()
}

/// The kind of the symbol at `spot`. The definition the server gives names the
/// declaration; a declaration that no member symbol covers is a local variable or a
/// parameter, which takes the readers and writers flags of a field.
async fn target_of_symbol(
    support: &dyn LanguageSupport,
    spot: &DocumentSpot,
) -> Option<FindUsagesTarget> {
    let mut self_definition = false;
    if let Some(provider) = support.navigation() {
        let mut replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::Definition,
                spot: spot.clone(),
            },
        );
        if let Some(answer) = replies.next().await {
            for place in answer.value {
                if place.uri == spot.uri && contains(&place.span, spot.point) {
                    self_definition = true;
                    continue;
                }
                if let Some(target) = target_of_declaration(support, &place).await {
                    return Some(target);
                }
                // A local variable or a parameter is declared in the same file.
                if place.uri == spot.uri {
                    return Some(FindUsagesTarget::Field);
                }
            }
        }
    }
    if let Some(symbol) = symbol_at(support, spot).await {
        if contains(&symbol.name_range, spot.point) {
            return Some(FindUsagesTarget::of(symbol.kind));
        }
        // The declaration under the caret is not a member of the file: it is a local
        // variable or a parameter, when the server points at this very place.
        if !self_definition {
            return Some(FindUsagesTarget::of(symbol.kind));
        }
    }
    self_definition.then_some(FindUsagesTarget::Field)
}

/// The kind of the declaration at `place`, when a symbol of its file covers its name.
async fn target_of_declaration(
    support: &dyn LanguageSupport,
    place: &lang_api::SourceSpan,
) -> Option<FindUsagesTarget> {
    declaration_symbol_at(support, place)
        .await
        .map(|symbol| FindUsagesTarget::of(symbol.kind))
}

/// The symbol at `place`, when a declaration of its file covers the name there.
async fn declaration_symbol_at(
    support: &dyn LanguageSupport,
    place: &lang_api::SourceSpan,
) -> Option<FileSymbol> {
    let provider = support.file_symbols()?;
    let mut replies = ask(provider, FileSymbolsQuery(place.uri.clone()));
    let answer = replies.next().await?;
    let symbol = symbol_at_point(&answer.value, place.span.start)?;
    contains(&symbol.name_range, place.span.start).then(|| symbol.clone())
}

/// The declaration the server points at, for its members and its name. A declaration
/// under the caret itself is read from the file symbols.
async fn declaration_symbol(
    support: &dyn LanguageSupport,
    spot: &DocumentSpot,
) -> Option<FileSymbol> {
    if let Some(provider) = support.navigation() {
        let mut replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::Definition,
                spot: spot.clone(),
            },
        );
        if let Some(answer) = replies.next().await {
            for place in answer.value {
                if place.uri == spot.uri && contains(&place.span, spot.point) {
                    continue;
                }
                if let Some(symbol) = declaration_symbol_at(support, &place).await {
                    return Some(symbol);
                }
            }
        }
    }
    symbol_at(support, spot).await
}

/// A closed file opened on the server for one answer of the usages search.
struct OpenForQuery<'a> {
    server: &'a dyn lang_api::SemanticServer,
    uris: Vec<String>,
}

impl Drop for OpenForQuery<'_> {
    fn drop(&mut self) {
        for uri in self.uris.drain(..) {
            self.server.close(&uri);
        }
    }
}

/// The uses of the declaration at `spot`, named `name` in the list. A file the server
/// does not hold open is opened for the answer and closed after it.
async fn usages_of_spot(
    support: &dyn LanguageSupport,
    mut spot: DocumentSpot,
    name: String,
    background: &ui_kit::gpui::BackgroundExecutor,
) -> Vec<DetailedUsage> {
    let mut opened = support.server().map(|server| OpenForQuery {
        server,
        uris: Vec::new(),
    });
    if let Some(opened) = &mut opened {
        match opened.server.version(&spot.uri) {
            // A document the editor holds open keeps its own version.
            Some(version) => spot.version = version,
            None => {
                if let Some(path) = uri_path(&spot.uri) {
                    // A closed file is read off the window thread, once for the query.
                    let read = background
                        .spawn(async move { fs::read_to_string(path).ok() })
                        .await;
                    if let Some(text) = read {
                        opened.server.open(&spot.uri, 1, text.into());
                        opened.uris.push(spot.uri.clone());
                        spot.version = 1;
                    }
                }
            }
        }
    }
    let entries = match support.usages() {
        Some(provider) => {
            let mut replies = ask(provider, spot);
            match replies.next().await {
                Some(answer) => answer.value,
                None => return Vec::new(),
            }
        }
        None => {
            let Some(provider) = support.references() else {
                return Vec::new();
            };
            let mut replies = ask(provider, spot);
            match replies.next().await {
                Some(answer) => answer
                    .value
                    .into_iter()
                    .map(|target| UsageEntry {
                        target,
                        kind: UsageKind::Unknown,
                        container: None,
                    })
                    .collect(),
                None => return Vec::new(),
            }
        }
    };
    entries
        .into_iter()
        .filter(|entry| entry.kind != UsageKind::Declaration)
        .map(|entry| {
            let module = uri_path(&entry.target.uri).and_then(|path| support.module_of(&path));
            DetailedUsage {
                name: name.clone(),
                target: entry.target,
                kind: entry.kind,
                container: entry.container,
                module,
            }
        })
        .collect()
}

fn display_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn split_name(name: &str) -> Vec<String> {
    name.split(['.', '/', '\\'])
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn header_buttons(settings: &Settings, cx: &mut Context<UsagesHub>) -> Vec<HeaderButton> {
    let weak = cx.weak_entity();
    let button = |id,
                  label: &str,
                  icon: &'static str,
                  tooltip: &str,
                  shortcut: &str,
                  active,
                  change: fn(&mut Settings)| {
        let weak = weak.clone();
        HeaderButton {
            id,
            label: label.to_owned(),
            active,
            icon: Some(icon),
            tooltip: Some(tooltip.to_owned()),
            shortcut: Some(shortcut.to_owned()),
            run: std::rc::Rc::new(move |_, cx| {
                let weak = weak.clone();
                cx.spawn(async move |cx| {
                    weak.update(cx, |hub, cx| hub.change_setting(change, cx))
                        .ok();
                })
                .detach();
            }),
        }
    };
    vec![
        button(
            "usage-group-type",
            "Group by Type",
            "general/filter",
            "Group by Type",
            "Ctrl+Alt+T",
            settings.usage_group_usage_type,
            |settings| settings.usage_group_usage_type = !settings.usage_group_usage_type,
        ),
        button(
            "usage-group-module",
            "Group by Module",
            "nodes/module",
            "Group by Module",
            "Ctrl+Alt+M",
            settings.usage_group_module,
            |settings| settings.usage_group_module = !settings.usage_group_module,
        ),
        button(
            "usage-group-directory",
            "Group by Package",
            "nodes/package",
            "Group by Package",
            "Ctrl+Alt+P",
            settings.usage_group_directory,
            |settings| settings.usage_group_directory = !settings.usage_group_directory,
        ),
        button(
            "usage-group-directory-structure",
            "Group by Directory",
            "nodes/folder",
            "Group by Directory Structure",
            "Ctrl+Alt+D",
            settings.usage_group_directory_structure,
            |settings| {
                settings.usage_group_directory_structure = !settings.usage_group_directory_structure
            },
        ),
        button(
            "usage-group-file-structure",
            "Group by File",
            "fileTypes/text",
            "Group by File Structure",
            "Ctrl+Alt+F",
            settings.usage_group_file_structure,
            |settings| settings.usage_group_file_structure = !settings.usage_group_file_structure,
        ),
        button(
            "usage-flatten-modules",
            "Flatten Modules",
            "general/collapseAll",
            "Flatten Modules",
            "Ctrl+Alt+O",
            settings.usage_flatten_modules,
            |settings| settings.usage_flatten_modules = !settings.usage_flatten_modules,
        ),
        button(
            "usage-filter-read",
            "Show Read Access",
            "general/show",
            "Show Read Access",
            "Ctrl+R",
            !settings.usage_show_read,
            |settings| settings.usage_show_read = !settings.usage_show_read,
        ),
        button(
            "usage-filter-write",
            "Show Write Access",
            "general/add",
            "Show Write Access",
            "Ctrl+W",
            !settings.usage_show_write,
            |settings| settings.usage_show_write = !settings.usage_show_write,
        ),
        button(
            "usage-filter-imports",
            "Show Imports",
            "general/openInToolWindow",
            "Show Imports",
            "Ctrl+I",
            !settings.usage_show_imports,
            |settings| settings.usage_show_imports = !settings.usage_show_imports,
        ),
    ]
}

/// Ctrl+Alt+Shift+F7: the scope and the flags of the next Find Usages.
pub struct FindUsagesSettings {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    options: FindUsagesOptions,
    selected: usize,
    /// The kind of the symbol under the caret, which decides the flags.
    kind: Option<FindUsagesTarget>,
}

impl FindUsagesSettings {
    pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let weak = cx.weak_entity();
        let options = cx
            .try_global::<PendingFindUsages>()
            .and_then(|pending| pending.0.clone())
            .unwrap_or_default();
        let selected = FindUsagesScope::ALL
            .iter()
            .position(|scope| *scope == options.scope)
            .unwrap_or(0);
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            options,
            selected,
            kind: None,
        });
        cx.set_global(ActiveFindUsagesSettings(Some(view.clone())));
        let found = view.clone();
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
        cx.spawn(async move |_, cx| {
            found.update(cx, |view, cx| view.detect_kind(cx));
        })
        .detach();
    }

    /// Asks the language what the caret is on, so the flags match the symbol.
    fn detect_kind(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some((_, view)) = workspace.read(cx).active_editor() else {
            return;
        };
        let Ok(view) = view.downcast::<EditorView>() else {
            return;
        };
        let Some(spot) = view.read(cx).spot().map(|(spot, _)| spot) else {
            return;
        };
        let Some(support) = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Some(kind) = target_of_symbol(&*support, &spot).await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.kind = Some(kind);
                this.options.target = Some(kind);
                // A field has no Usages flag: its search is not narrowed by one.
                if kind == FindUsagesTarget::Field {
                    this.options.usages = true;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Finds right away, as IDEA does, and keeps the flags for the next Alt+F7.
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.options.scope = FindUsagesScope::ALL[self.selected];
        self.options.target = self.kind;
        if self.kind == Some(FindUsagesTarget::Field) {
            self.options.usages = true;
        }
        cx.set_global(PendingFindUsages(Some(self.options.clone())));
        self.dismiss(window, cx);
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let Some((_, view)) = workspace.active_editor() else {
                        return;
                    };
                    let Ok(view) = view.downcast::<EditorView>() else {
                        return;
                    };
                    view.update(cx, |view, cx| view.find_usages(cx));
                })
                .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_global(ActiveFindUsagesSettings(None));
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = FindUsagesScope::ALL.len() - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last as isize) as usize;
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "enter" => self.apply(window, cx),
            "escape" => self.dismiss(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// A row of the flags of the dialog: its id, its label, its state and its toggle.
type FlagRow = (
    &'static str,
    &'static str,
    bool,
    fn(&mut FindUsagesSettings),
);

fn check_row(
    id: &'static str,
    label: &'static str,
    on: bool,
    theme: &Theme,
    toggle: impl Fn(&mut FindUsagesSettings) + 'static,
    cx: &mut Context<FindUsagesSettings>,
) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .debug_selector(move || format!("find-usages-{}", id.replace('_', "-")))
        .flex()
        .items_center()
        .gap(px(SPACE_S))
        .h(px(ROW_HEIGHT))
        .px(px(ROW_PADDING_X))
        .rounded(px(ROW_RADIUS))
        .hover(move |style| style.bg(hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            toggle(this);
            cx.notify();
        }))
        .child(
            checkbox(on, theme)
                .debug_selector(move || format!("find-usages-check-{}", id.replace('_', "-"))),
        )
        .child(label)
}

impl Render for FindUsagesSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(420.);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.2;
        let hover = theme.hover;
        let scopes: Vec<_> = FindUsagesScope::ALL
            .iter()
            .enumerate()
            .map(|(index, scope)| {
                let chosen = index == self.selected;
                div()
                    .id(("find-usages-scope", index))
                    .debug_selector(move || format!("find-usages-scope-{index}"))
                    .flex()
                    .items_center()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
                    .child(scope.label())
            })
            .collect();
        let mut flags = Vec::new();
        // A field or a variable has no Usages flag: readers and writers replace it.
        if self.kind != Some(FindUsagesTarget::Field) {
            flags.push(check_row(
                "usages",
                "Usages",
                self.options.usages,
                &theme,
                |this| this.options.usages = !this.options.usages,
                cx,
            ));
        }
        match self.kind {
            Some(FindUsagesTarget::Class) => {
                flags.push(check_row(
                    "methods",
                    "Usages of methods",
                    self.options.methods,
                    &theme,
                    |this| this.options.methods = !this.options.methods,
                    cx,
                ));
                flags.push(check_row(
                    "fields",
                    "Usages of fields",
                    self.options.fields,
                    &theme,
                    |this| this.options.fields = !this.options.fields,
                    cx,
                ));
                flags.push(check_row(
                    "derived",
                    "Implementing/derived classes",
                    self.options.derived,
                    &theme,
                    |this| this.options.derived = !this.options.derived,
                    cx,
                ));
            }
            Some(FindUsagesTarget::Method) => {
                flags.push(check_row(
                    "overriding",
                    "Overriding methods",
                    self.options.overriding,
                    &theme,
                    |this| this.options.overriding = !this.options.overriding,
                    cx,
                ));
                flags.push(check_row(
                    "implementing",
                    "Implementing methods",
                    self.options.implementing,
                    &theme,
                    |this| this.options.implementing = !this.options.implementing,
                    cx,
                ));
            }
            Some(FindUsagesTarget::Field) => {
                flags.push(check_row(
                    "readers",
                    "Readers",
                    self.options.readers,
                    &theme,
                    |this| this.options.readers = !this.options.readers,
                    cx,
                ));
                flags.push(check_row(
                    "writers",
                    "Writers",
                    self.options.writers,
                    &theme,
                    |this| this.options.writers = !this.options.writers,
                    cx,
                ));
            }
            None => {
                let all: [FlagRow; 7] = [
                    (
                        "methods",
                        "Usages of methods",
                        self.options.methods,
                        |this| this.options.methods = !this.options.methods,
                    ),
                    ("fields", "Usages of fields", self.options.fields, |this| {
                        this.options.fields = !this.options.fields
                    }),
                    (
                        "derived",
                        "Implementing/derived classes",
                        self.options.derived,
                        |this| this.options.derived = !this.options.derived,
                    ),
                    (
                        "overriding",
                        "Overriding methods",
                        self.options.overriding,
                        |this| this.options.overriding = !this.options.overriding,
                    ),
                    (
                        "implementing",
                        "Implementing methods",
                        self.options.implementing,
                        |this| this.options.implementing = !this.options.implementing,
                    ),
                    ("readers", "Readers", self.options.readers, |this| {
                        this.options.readers = !this.options.readers
                    }),
                    ("writers", "Writers", self.options.writers, |this| {
                        this.options.writers = !this.options.writers
                    }),
                ];
                for (id, label, on, toggle) in all {
                    flags.push(check_row(id, label, on, &theme, toggle, cx));
                }
            }
        }
        let body = surface(&theme)
            .id("find-usages-settings")
            .debug_selector(|| "find-usages-settings".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(SPACE_S))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .pb(px(SPACE_S))
                    .child("Find Usages"),
            )
            .child(div().text_color(theme.text_secondary).child("Search in"))
            .children(scopes)
            .child(
                div()
                    .mt(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child("Options"),
            )
            .children(flags)
            .child(check_row(
                "new_tab",
                "Open in new tab",
                self.options.open_in_new_tab,
                &theme,
                |this| this.options.open_in_new_tab = !this.options.open_in_new_tab,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_S))
                    .mt(px(SPACE_M))
                    .child(
                        div()
                            .id("find-usages-cancel")
                            .debug_selector(|| "find-usages-cancel".to_owned())
                            .px(px(SPACE_M))
                            .h(px(ROW_HEIGHT))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.control_border)
                            .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .id("find-usages-find")
                            .debug_selector(|| "find-usages-find".to_owned())
                            .px(px(SPACE_M))
                            .h(px(ROW_HEIGHT))
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.accent)
                            .text_color(theme.over_accent)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)))
                            .child("Find"),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("find-usages-enter", body))
    }
}

/// One place in the popup, with the text of its line ready to draw.
struct Row {
    path: PathBuf,
    relative: String,
    line: usize,
    column: usize,
    text: String,
    highlight: Range<usize>,
    /// The place in the line, kept until the text of the line arrives.
    start: u32,
    end: u32,
    end_line: u32,
    /// The line was read, so the row needs no more work.
    loaded: bool,
    /// A background read is on its way for this row.
    loading: bool,
}

/// One line of a file, sent from the reader to the window.
type LineResult = (u64, usize, PathBuf, usize, String);

/// The Show Usages popup: the places of one symbol, filtered by typing.
pub struct ShowUsagesPopup {
    workspace: WeakEntity<Workspace>,
    root: PathBuf,
    current: PathBuf,
    focus: FocusHandle,
    position: Point<Pixels>,
    scroll: ScrollHandle,
    name: String,
    usages: Vec<Usage>,
    rows: Vec<Row>,
    /// The rows that pass the filter, as indices into `rows`.
    shown: Vec<usize>,
    query: TextField,
    selected: usize,
    done: bool,
    /// Bumped when a new search starts, so late lines of the old one are dropped.
    generation: u64,
}

/// The line of a file that is open in an editor, without touching the disk.
fn open_line(path: &Path, line: usize, cx: &App) -> Option<String> {
    let editor = OpenEditors::get(cx, path)?;
    editor.read(cx).line_text(line)
}

/// The byte offset of a UTF-16 column in one line, as the language counts columns.
fn byte_at(line: &str, column: u32) -> usize {
    let mut units = 0;
    for (byte, character) in line.char_indices() {
        if units >= column as usize {
            return byte;
        }
        units += character.len_utf16();
    }
    line.len()
}

impl Row {
    /// Takes the line of the file, cutting the snippet and marking the place.
    fn set_text(&mut self, line: &str) {
        let text: String = line.chars().take(SNIPPET).collect();
        let start = byte_at(&text, self.start);
        let end = if self.end_line as usize == self.line {
            byte_at(&text, self.end)
        } else {
            text.len()
        };
        let end = end.max(start).min(text.len());
        self.text = text;
        self.highlight = start..end;
        self.loaded = true;
        self.loading = false;
    }
}

/// Reads one file once and sends the lines the popup asked for.
fn read_file(
    generation: u64,
    path: PathBuf,
    lines: Vec<(usize, usize)>,
    sender: UnboundedSender<LineResult>,
) {
    #[cfg(test)]
    BACKGROUND_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let text = read_text(&path);
    for (index, line) in lines {
        let text = text
            .as_deref()
            .and_then(|text| text.lines().nth(line))
            .unwrap_or_default()
            .to_owned();
        if sender
            .unbounded_send((generation, index, path.clone(), line, text))
            .is_err()
        {
            return;
        }
    }
}

/// Feeds the lines of the reader to the popup as they arrive. A batch updates the
/// window once, so hundreds of lines do not repaint the popup for each of them.
async fn receive_lines(
    popup: WeakEntity<ShowUsagesPopup>,
    mut receiver: UnboundedReceiver<LineResult>,
    cx: &mut AsyncApp,
) {
    while let Some(first) = receiver.next().await {
        let mut batch = vec![first];
        while let Ok(line) = receiver.try_recv() {
            batch.push(line);
        }
        let updated = popup.update(cx, |popup, cx| {
            for (generation, index, path, line, text) in batch {
                popup.fill(generation, index, &path, line, text);
            }
            cx.notify();
        });
        if updated.is_err() {
            return;
        }
    }
}

/// A place that the one-usage shortcut opens after the current update.
struct Target {
    path: PathBuf,
    line: usize,
    column: usize,
}

impl ShowUsagesPopup {
    /// A row of one usage with the text of its line, when an editor has it.
    fn row_of(&self, usage: &Usage, cx: &App) -> Option<Row> {
        let path = uri_path(&usage.target.uri)?;
        let line = usage.target.span.start.line as usize;
        let relative = path
            .strip_prefix(&self.root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");
        let mut row = Row {
            path,
            relative,
            line,
            column: usage.target.span.start.character as usize,
            text: String::new(),
            highlight: 0..0,
            start: usage.target.span.start.character,
            end: usage.target.span.end.character,
            end_line: usage.target.span.end.line,
            loaded: false,
            loading: false,
        };
        if let Some(text) = open_line(&row.path, row.line, cx) {
            row.set_text(&text);
        }
        Some(row)
    }

    /// Takes an answer of the search. The popup keeps what it has until the search is
    /// done; the one place of the answer is returned so the caller can open it outside
    /// the update that delivered the answer.
    fn receive(&mut self, update: UsagesUpdate, cx: &mut Context<Self>) -> Option<Target> {
        if update.name != self.name || (!update.done && update.usages.is_empty()) {
            self.query = TextField::default();
            self.selected = 0;
            self.rows.clear();
            self.shown.clear();
            self.generation += 1;
        }
        self.name = update.name;
        self.done = update.done;
        self.usages = update.usages;
        let current = self.current.clone();
        let key_of = |usage: &Usage| {
            let path = uri_path(&usage.target.uri)?;
            Some((
                path,
                usage.target.span.start.line,
                usage.target.span.start.character,
                usage.target.span.end.line,
                usage.target.span.end.character,
            ))
        };
        let mut wanted: HashMap<_, usize> = HashMap::new();
        let mut have: HashMap<_, usize> = HashMap::new();
        for row in &self.rows {
            let key = (
                row.path.clone(),
                row.line as u32,
                row.start,
                row.end_line,
                row.end,
            );
            *have.entry(key).or_default() += 1;
        }
        for usage in &self.usages {
            let Some(key) = key_of(usage) else {
                continue;
            };
            let wanted_count = wanted.entry(key.clone()).or_default();
            *wanted_count += 1;
            if have.get(&key).is_some_and(|count| count >= &*wanted_count) {
                continue;
            }
            let Some(row) = self.row_of(usage, cx) else {
                continue;
            };
            *have.entry(key).or_default() += 1;
            let index = if row.path == current {
                self.rows.partition_point(|other| other.path == current)
            } else {
                self.rows.len()
            };
            self.rows.insert(index, row);
        }
        self.refilter();
        self.request_lines(cx);
        if self.done && self.rows.len() == 1 {
            let row = &self.rows[0];
            return Some(Target {
                path: row.path.clone(),
                line: row.line,
                column: row.column,
            });
        }
        cx.notify();
        None
    }

    /// Reads the lines that no editor holds in the background, one task per file.
    fn request_lines(&mut self, cx: &mut Context<Self>) {
        let mut files: HashMap<PathBuf, Vec<(usize, usize)>> = HashMap::new();
        for (index, row) in self.rows.iter_mut().enumerate() {
            if row.loaded || row.loading {
                continue;
            }
            row.loading = true;
            files
                .entry(row.path.clone())
                .or_default()
                .push((index, row.line));
        }
        let generation = self.generation;
        for (path, lines) in files {
            let (sender, receiver) = unbounded();
            let read_path = path.clone();
            cx.background_executor()
                .spawn(async move {
                    read_file(generation, read_path, lines, sender);
                })
                .detach();
            cx.spawn(async move |this, cx| {
                receive_lines(this, receiver, cx).await;
            })
            .detach();
        }
    }

    /// Puts one line into its row, without touching the rows that are ready.
    fn fill(&mut self, generation: u64, index: usize, path: &Path, line: usize, text: String) {
        if generation != self.generation {
            return;
        }
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if row.path != path || row.line != line {
            return;
        }
        row.set_text(&text);
        self.refilter();
    }

    fn refilter(&mut self) {
        let query = self.query.text.to_lowercase();
        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row.text.to_lowercase().contains(&query)
                    || row.relative.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        if self.selected >= self.shown.len() {
            self.selected = 0;
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_global(ActiveShowUsages(None));
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&row_at) = self.shown.get(index) else {
            return;
        };
        let row = &self.rows[row_at];
        let (path, line, column) = (row.path.clone(), row.line, row.column);
        cx.set_global(ActiveShowUsages(None));
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                let spot = Spot::Line { line, column };
                navigation::go_to(workspace, path, Some(spot), true, window, cx);
            })
            .ok();
    }

    fn open_in_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hub) = cx.try_global::<ActiveUsages>() {
            let hub = hub.0.clone();
            let usages = self.usages.clone();
            hub.update(cx, |hub, cx| hub.set_usages(usages, cx));
        }
        cx.set_global(ActiveShowUsages(None));
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.show_tool(USAGES_TOOL, window, cx);
            })
            .ok();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last as isize) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "f7" && key.modifiers.control && key.modifiers.alt {
            cx.stop_propagation();
            return self.open_in_window(window, cx);
        }
        match key.key.as_str() {
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "enter" => self.open(self.selected, window, cx),
            "escape" => self.dismiss(window, cx),
            _ => {
                if !self.query.key_with_clipboard(event, false, cx) {
                    return;
                }
                self.selected = 0;
                self.refilter();
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// The number of rows that pass the filter, for tests.
    #[cfg(test)]
    pub(crate) fn visible_rows(&self) -> usize {
        self.shown.len()
    }

    /// Whether the search sent its last answer, for tests.
    #[cfg(test)]
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// How many rows already have the text of their line, for tests.
    #[cfg(test)]
    pub(crate) fn loaded_rows(&self) -> usize {
        self.rows.iter().filter(|row| row.loaded).count()
    }

    /// The row count, for tests.
    #[cfg(test)]
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Opens the only place after the update that delivered it is over.
fn open_later(target: Target, window: &mut Window, cx: &mut Context<Workspace>) {
    cx.set_global(ActiveShowUsages(None));
    let handle = window.window_handle();
    let workspace = cx.weak_entity();
    cx.defer(move |cx| {
        handle
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.dismiss(window, cx);
                        let spot = Spot::Line {
                            line: target.line,
                            column: target.column,
                        };
                        navigation::go_to(workspace, target.path, Some(spot), true, window, cx);
                    })
                    .ok();
            })
            .ok();
    });
}

/// Shows the popup, or feeds a later answer to the one already on screen.
pub fn show_usages(
    update: UsagesUpdate,
    current: PathBuf,
    position: Option<Point<Pixels>>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = cx
        .try_global::<ActiveShowUsages>()
        .and_then(|active| active.0.clone());
    if let Some(popup) = existing {
        // The first event of a search makes sure the popup is the one on screen even
        // if another overlay took its place in the meantime.
        if update.usages.is_empty() && !update.done {
            let focus = popup.read(cx).focus.clone();
            workspace.show_overlay(popup.clone().into(), focus, window, cx);
        }
        let target = popup.update(cx, |popup, cx| popup.receive(update, cx));
        if let Some(target) = target {
            open_later(target, window, cx);
        }
        return;
    }
    // The answer of a search that was closed before it came in changes nothing.
    if !update.usages.is_empty() || update.done {
        return;
    }
    let root = workspace.root().to_path_buf();
    let weak = cx.weak_entity();
    let position = position.unwrap_or_else(|| point(px(120.), px(120.)));
    let view = cx.new(|cx| ShowUsagesPopup {
        workspace: weak,
        root,
        current,
        focus: cx.focus_handle(),
        position,
        scroll: ScrollHandle::new(),
        name: update.name.clone(),
        usages: Vec::new(),
        rows: Vec::new(),
        shown: Vec::new(),
        query: TextField::default(),
        selected: 0,
        done: false,
        generation: 0,
    });
    cx.set_global(ActiveShowUsages(Some(view.clone())));
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.clone().into(), focus, window, cx);
    let target = view.update(cx, |popup, cx| popup.receive(update, cx));
    if let Some(target) = target {
        open_later(target, window, cx);
    }
}

impl Render for ShowUsagesPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let searching = !self.done;
        let title = format!("Usages of \"{}\"", self.name);
        let status = if searching {
            "Searching...".to_owned()
        } else {
            format!("{} found", self.rows.len())
        };
        let open_in_window = div()
            .id("usages-open-window")
            .debug_selector(|| "usages-open-window".to_owned())
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(SPACE_M))
            .flex()
            .items_center()
            .rounded(px(CONTROL_RADIUS))
            .text_color(theme.accent)
            .hover(move |style| style.bg(theme.hover))
            .on_click(cx.listener(|this, _, window, cx| this.open_in_window(window, cx)))
            .child("Open in Find Window");
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .gap(px(SPACE_M))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .min_w_0()
                    .child(icon("codeInsight/search", &theme))
                    .child(div().truncate().child(title)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "usages-status".to_owned())
                            .text_color(theme.text_secondary)
                            .child(status),
                    )
                    .child(open_in_window),
            );
        let filter = (!self.query.text.is_empty()).then(|| {
            div()
                .debug_selector(|| "usages-search".to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .px(px(SPACE_M))
                .pb(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child("Filter:")
                .child(div().flex().items_center().whitespace_nowrap().children(
                    text_field_children(
                        &self.query,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        true,
                    ),
                ))
        });
        let rows: Vec<_> = self
            .shown
            .clone()
            .into_iter()
            .enumerate()
            .map(|(at, row_at)| {
                let row = &self.rows[row_at];
                let selected = at == self.selected;
                let before = row.text[..row.highlight.start].to_owned();
                let found = row.text[row.highlight.clone()].to_owned();
                let after = row.text[row.highlight.end..].to_owned();
                div()
                    .id(("usages-row", at))
                    .debug_selector(move || format!("usages-row-{at}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .mx(px(SPACE_M))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(move |style| style.bg(theme.selection_hover))
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.selected = at;
                        if event.click_count() >= 2 {
                            this.open(at, window, cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .child(icon("codeInsight/search", &theme))
                    .child(
                        div()
                            .w(px(FILE_WIDTH))
                            .flex_none()
                            .truncate()
                            .text_color(theme.text_secondary)
                            .child(format!("{}:{}", row.relative, row.line + 1)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(before)
                            .child(
                                div()
                                    .bg(theme.match_bg)
                                    .text_color(theme.match_text)
                                    .rounded(px(2.))
                                    .child(found),
                            )
                            .child(after),
                    )
            })
            .collect();
        let empty = rows.is_empty().then(|| {
            let (text, selector) = if searching {
                ("Searching for usages...", "usages-searching")
            } else {
                ("No usages found", "usages-empty")
            };
            div()
                .debug_selector(move || selector.to_owned())
                .flex()
                .items_center()
                .justify_center()
                .h(px(72.))
                .text_color(theme.text_secondary)
                .child(text)
        });
        let body = surface(&theme)
            .id("usages-popup")
            .debug_selector(|| "usages-popup".to_owned())
            .key_context(POPUP_CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .w(px(POPUP_WIDTH))
            .max_h(px(POPUP_HEIGHT))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(header)
            .children(filter)
            .child(
                scrollable(
                    "usages-list",
                    &self.scroll,
                    div()
                        .id("usages-list")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows)
                        .children(empty),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        let viewport = window.viewport_size();
        let width = px(POPUP_WIDTH);
        let left = self
            .position
            .x
            .min(viewport.width - width - px(SPACE_M))
            .max(px(SPACE_M));
        let top = self
            .position
            .y
            .min(viewport.height - px(80.))
            .max(px(SPACE_M));
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("usages-enter", body))
    }
}
