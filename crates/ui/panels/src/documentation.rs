//! The Documentation tool window: the rendered answer for the symbol at the caret. The
//! answer follows the caret after a pause, a late answer is dropped, and Back and Forward
//! walk the answers seen before.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

#[cfg(test)]
use editor_view::display_documentation;
use editor_view::{
    DocBlock, DocHost, DocLinkHandler, EditorStyle, EditorView, Spot, parse_documentation,
    render_document, split_line_fragment, uri_path,
};
use futures_util::StreamExt;
use lang_api::ask;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DOCUMENTATION_TOOL, Workspace};

use crate::navigation;
use crate::outline::OpenEditors;

const GROUP: &str = "right-panel";
/// The pause before the answer for a new caret place is asked.
const QUERY_DELAY: Duration = Duration::from_millis(250);
/// The answers kept for Back and Forward.
const MAX_HISTORY: usize = 50;

/// One answer, as the window shows it.
#[derive(Clone)]
struct DocEntry {
    blocks: Vec<DocBlock>,
    diagnostic: Option<String>,
}

pub struct DocumentationPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: Option<PathBuf>,
    editor: Option<WeakEntity<EditorView>>,
    doc: Option<DocEntry>,
    history: Vec<DocEntry>,
    index: usize,
    /// The offset of the last question, so a moved caret asks and a still one does not.
    last_offset: Option<usize>,
    /// Grows with every question, so only the last answer reaches the window.
    asked: u64,
    scroll: ScrollHandle,
    _editor: Option<Subscription>,
}

/// Lets the window show the panel for the active editor.
pub struct ActiveDocumentation(pub Entity<DocumentationPanel>);

impl Global for ActiveDocumentation {}

impl DocumentationPanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            path: None,
            editor: None,
            doc: None,
            history: Vec::new(),
            index: 0,
            last_offset: None,
            asked: 0,
            scroll: ScrollHandle::new(),
            _editor: None,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The text of the answer on screen, for the tests.
    #[cfg(test)]
    pub fn documentation_text(&self) -> Option<String> {
        self.doc
            .as_ref()
            .map(|doc| display_documentation(&doc.blocks, doc.diagnostic.as_deref()))
    }

    /// The active file became another one: the window follows its editor.
    pub fn show_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_ref() == Some(&path) {
            return;
        }
        let editor = OpenEditors::get(cx, &path);
        self.attach(editor, path, cx);
    }

    /// The editor the caret is in: the window listens to it and asks for its answer.
    pub fn show_editor(&mut self, editor: Entity<EditorView>, cx: &mut Context<Self>) {
        let path = editor.read(cx).path().to_path_buf();
        self.attach(Some(editor), path, cx);
    }

    fn attach(
        &mut self,
        editor: Option<Entity<EditorView>>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self._editor = editor.as_ref().map(|editor| {
            cx.observe(editor, |this, editor, cx| {
                if editor.read(cx).head_offset() != this.last_offset {
                    this.schedule(cx);
                }
            })
        });
        self.editor = editor.map(|editor| editor.downgrade());
        self.path = Some(path);
        self.last_offset = None;
        self.schedule(cx);
    }

    /// The caret may have moved: asks for the answer once the movement settles.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        self.asked += 1;
        let serial = self.asked;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUERY_DELAY).await;
            let query = this
                .update(cx, |this, cx| {
                    if this.asked != serial || !this.is_open(cx) {
                        return None;
                    }
                    let query = editor.read(cx).documentation_query()?;
                    if this.last_offset == Some(query.offset) && this.doc.is_some() {
                        return None;
                    }
                    this.last_offset = Some(query.offset);
                    Some(query)
                })
                .ok()
                .flatten();
            let Some(query) = query else {
                return;
            };
            let Some(provider) = query.support.hover() else {
                return;
            };
            let mut replies = ask(provider, query.spot);
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                if this.asked != serial {
                    return;
                }
                let Some(answer) = answer else {
                    return;
                };
                let text = answer.value.0;
                if text.trim().is_empty() {
                    this.doc = None;
                    cx.notify();
                    return;
                }
                let blocks = parse_documentation(&text);
                this.push(
                    DocEntry {
                        blocks,
                        diagnostic: query.diagnostic,
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn is_open(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).tool_open(DOCUMENTATION_TOOL))
    }

    fn push(&mut self, entry: DocEntry, cx: &mut Context<Self>) {
        if self.index + 1 < self.history.len() {
            self.history.truncate(self.index + 1);
        }
        self.history.push(entry.clone());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.index = self.history.len().saturating_sub(1);
        self.doc = Some(entry);
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.index == 0 {
            return;
        }
        self.index -= 1;
        self.doc = self.history.get(self.index).cloned();
        cx.notify();
    }

    fn forward(&mut self, cx: &mut Context<Self>) {
        if self.index + 1 >= self.history.len() {
            return;
        }
        self.index += 1;
        self.doc = self.history.get(self.index).cloned();
        cx.notify();
    }

    /// A link of the answer: a file goes the way Go to Declaration goes, a web address
    /// opens in the browser.
    fn open_link(&mut self, url: String, window: &mut Window, cx: &mut App) {
        if url.starts_with("http://") || url.starts_with("https://") {
            let custom = cx
                .try_global::<editor_view::ExternalLinks>()
                .and_then(|links| links.0.clone());
            match custom {
                Some(open) => open(&url),
                None => {
                    ::platform::open_url(&url);
                }
            }
            return;
        }
        let (uri, line) = split_line_fragment(&url);
        let Some(path) = uri_path(&uri) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            navigation::go_to(
                workspace,
                path,
                Some(Spot::Line {
                    line: line as usize,
                    column: 0,
                }),
                true,
                window,
                cx,
            )
        });
    }
}

/// Back and Forward: an arrow of the header, dimmed while there is nowhere to go.
fn history_button(
    id: &'static str,
    icon: &str,
    label: &'static str,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    icon_button(id, icon, label, None, theme)
        .debug_selector(move || id.to_owned())
        .when(!enabled, |this| this.opacity(0.4))
}

impl Render for DocumentationPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let style = EditorStyle::from_theme(&theme);
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(DOCUMENTATION_TOOL, window, cx)
                })
                .ok();
        };
        let back = history_button(
            "doc-back",
            "general/arrowLeft",
            "Back",
            self.index > 0,
            &theme,
        );
        let back = if self.index > 0 {
            back.on_click(cx.listener(|this, _, _, cx| this.back(cx)))
        } else {
            back
        };
        let forward = history_button(
            "doc-forward",
            "general/arrowRight",
            "Forward",
            self.index + 1 < self.history.len(),
            &theme,
        );
        let forward = if self.index + 1 < self.history.len() {
            forward.on_click(cx.listener(|this, _, _, cx| this.forward(cx)))
        } else {
            forward
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .child(back)
                    .child(forward)
                    .child("Documentation"),
            )
            .child(panel_tools(GROUP, hide, &theme));
        let body = match &self.doc {
            Some(entry) => {
                let weak = cx.entity().downgrade();
                let on_link: DocLinkHandler = Rc::new(move |url, window, cx| {
                    weak.update(cx, |this, cx| this.open_link(url, window, cx))
                        .ok();
                });
                let host = DocHost {
                    name: "doc-window".to_owned(),
                    on_link,
                    on_select: None,
                    selection: None,
                };
                let content =
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .p(px(SPACE_M))
                        .child(render_document(
                            &entry.blocks,
                            entry.diagnostic.as_deref(),
                            &style,
                            &theme,
                            &host,
                            window,
                            cx,
                        ));
                scrollable(
                    "doc-window-rows",
                    &self.scroll,
                    div()
                        .id("doc-window-scroll")
                        .flex()
                        .flex_col()
                        .w_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(content),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0()
                .into_any_element()
            }
            None => div()
                .flex_1()
                .p(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child("Place the caret on a symbol")
                .into_any_element(),
        };
        island(&theme)
            .id(DOCUMENTATION_TOOL)
            .debug_selector(|| "documentation-window".to_owned())
            .key_context("DocumentationWindow ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(header)
            .child(body)
    }
}
