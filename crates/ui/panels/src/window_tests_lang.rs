//! The window with a language behind it: a support that answers from memory the way a
//! server does. Every scenario checks what the window did with the answer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use editor_view::{Languages, uri_path};
use keymap::workspace as keys;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, CodeLensCommand, CodeLensEntry,
    CodeLensProvider, CodeLensQuery, CodeLensResolveProvider, CodeLensResolveQuery,
    CompletionEntry, CompletionList, CompletionProvider, Confidence, DiagnosticEntry,
    DiagnosticsProvider, DiagnosticsQuery, DocumentEdit, DocumentSpot, FormatProvider, FormatQuery,
    InlayHintEntry, InlayHintKind, InlayHintProvider, InlayHintQuery, LanguageFn, LanguageId,
    LanguageSupport, MoveDestination, MoveDestinationProvider, MoveOutcome, MoveProvider,
    MoveQuery, MoveRequest, NavigationProvider, NavigationQuery, Provider, Queries,
    ReferencesProvider, RenameProvider, RenameQuery, SemanticServer, Severity, SourceEdit,
    SourceId, SourceSpan, SyntaxProvider, TextPoint, TextSpan, WorkspaceChange, offset_in,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, Theme, point, px};
use workspace::{FileIcons, Launch, Layout, PROBLEMS_TOOL, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::problems::ActiveProblems;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, draw, editor_of, line_height, open_palette, open_path,
    press, project_setup, scratch, type_text,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-server");

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

pub(crate) fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

pub(crate) fn edit_in(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: span_in(text, start, end),
        text: new_text.to_owned(),
    }
}

/// Where the replaced piece of a text begins and ends, and how long the new piece is.
fn replaced_piece(old: &str, new: &str) -> (usize, usize, usize) {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(before, after)| before == after)
        .count();
    let prefix = (0..=prefix)
        .rev()
        .find(|at| old.is_char_boundary(*at) && new.is_char_boundary(*at))
        .unwrap_or(0);
    let rest_old = old.len() - prefix;
    let rest_new = new.len() - prefix;
    let suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(before, after)| before == after)
        .count()
        .min(rest_old)
        .min(rest_new);
    let suffix = (0..=suffix)
        .rev()
        .find(|at| old.is_char_boundary(old.len() - *at) && new.is_char_boundary(new.len() - *at))
        .unwrap_or(0);
    (prefix, old.len() - suffix, new.len() - suffix)
}

/// A fake server keeps the hints it was given while the text under them changes, the
/// way a client that sends edits to its server moves them.
fn move_inlays(hints: &mut Vec<(usize, String, InlayHintKind)>, old: &str, new: &str) {
    if old == new {
        return;
    }
    let (start, old_end, new_end) = replaced_piece(old, new);
    hints.retain_mut(|(offset, _, _)| {
        if *offset >= old_end {
            *offset = new_end + (*offset - old_end);
            true
        } else {
            *offset <= start
        }
    });
}

/// A fake server moves the labels it keeps along the edits, as a client that sends
/// changes to its server sees them.
fn move_code_lens(lenses: &mut Vec<(usize, String, String)>, old: &str, new: &str) {
    if old == new {
        return;
    }
    let (start, old_end, new_end) = replaced_piece(old, new);
    lenses.retain_mut(|(offset, _, _)| {
        if *offset >= old_end {
            *offset = new_end + (*offset - old_end);
            true
        } else {
            *offset <= start
        }
    });
}

fn word_at(text: &str, point: TextPoint) -> Option<String> {
    let offset = offset_in(text, point);
    let mut start = offset;
    while start > 0 {
        let character = text[..start].chars().next_back()?;
        if !is_word(character) {
            break;
        }
        start -= character.len_utf8();
    }
    let mut end = offset;
    while end < text.len() {
        let character = text[end..].chars().next()?;
        if !is_word(character) {
            break;
        }
        end += character.len_utf8();
    }
    (start < end).then(|| text[start..end].to_owned())
}

/// Every whole-word occurrence of `word` replaced by `new_word`, in the coordinates of
/// the text before the edits.
fn replace_word(text: &str, word: &str, new_word: &str) -> Vec<DocumentEdit> {
    let mut edits = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let start = from + found;
        let end = start + word.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            edits.push(edit_in(text, start, end, new_word));
        }
        from = end;
    }
    edits
}

pub(crate) fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

/// The language of the window tests: every answer is set by the test, no process runs.
pub(crate) struct FakeLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    definition: Mutex<Option<SourceSpan>>,
    diagnostics: Mutex<Vec<DiagnosticEntry>>,
    actions: Mutex<Vec<ActionEntry>>,
    references: Mutex<Vec<SourceSpan>>,
    /// Hints by the offset of their place in the text.
    inlays: Mutex<Vec<(usize, String, InlayHintKind)>>,
    /// Labels by the offset of their declaration, with the command of their click.
    code_lens: Mutex<Vec<(usize, String, String)>>,
    destinations: Mutex<Vec<MoveDestination>>,
    move_target: Mutex<Option<PathBuf>>,
    closed: Mutex<Option<PathBuf>>,
    restarts: AtomicUsize,
}

impl FakeLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            definition: Mutex::new(None),
            diagnostics: Mutex::new(Vec::new()),
            actions: Mutex::new(Vec::new()),
            references: Mutex::new(Vec::new()),
            inlays: Mutex::new(Vec::new()),
            code_lens: Mutex::new(Vec::new()),
            destinations: Mutex::new(Vec::new()),
            move_target: Mutex::new(None),
            closed: Mutex::new(None),
            restarts: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set_definition(&self, path: &Path, line: u32, character: u32) {
        *self.definition.lock().unwrap() = Some(SourceSpan {
            uri: uri_of(path),
            span: TextSpan {
                start: TextPoint { line, character },
                end: TextPoint { line, character },
            },
        });
    }

    pub(crate) fn set_diagnostics(&self, entries: Vec<DiagnosticEntry>) {
        *self.diagnostics.lock().unwrap() = entries;
    }

    pub(crate) fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    pub(crate) fn set_references(&self, spans: Vec<SourceSpan>) {
        *self.references.lock().unwrap() = spans;
    }

    /// The hints the fake server answers, by the byte offset of their place.
    pub(crate) fn set_inlays(&self, hints: Vec<(usize, String, InlayHintKind)>) {
        *self.inlays.lock().unwrap() = hints;
    }

    /// The labels the fake server answers, by the offset of their declaration and with
    /// the command of a click.
    pub(crate) fn set_code_lens(&self, lenses: Vec<(usize, String, String)>) {
        *self.code_lens.lock().unwrap() = lenses;
    }

    pub(crate) fn set_move(&self, target: PathBuf, destinations: Vec<MoveDestination>) {
        *self.move_target.lock().unwrap() = Some(target);
        *self.destinations.lock().unwrap() = destinations;
    }

    /// A file the server changes without it being open in a tab.
    fn set_closed(&self, path: PathBuf) {
        *self.closed.lock().unwrap() = Some(path);
    }

    fn restarts(&self) -> usize {
        self.restarts.load(Ordering::Relaxed)
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }

    fn text_of(&self, uri: &str) -> Option<String> {
        if let Some(text) = self.documents.lock().unwrap().get(uri) {
            return Some(text.to_string());
        }
        uri_path(uri).and_then(|path| fs::read_to_string(path).ok())
    }
}

impl SemanticServer for FakeLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        let old = self
            .documents
            .lock()
            .unwrap()
            .insert(uri.to_owned(), text.clone());
        if let Some(old) = old {
            move_inlays(&mut self.inlays.lock().unwrap(), &old, &text);
            move_code_lens(&mut self.code_lens.lock().unwrap(), &old, &text);
        }
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .contains_key(uri)
            .then_some(1)
    }
}

impl Provider<DocumentSpot, CompletionList> for FakeLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<CompletionList>) {
        let entry = |label: &str, detail: &str| CompletionEntry {
            label: label.to_owned(),
            detail: Some(detail.to_owned()),
            insert_text: Some(label.to_owned()),
            ..CompletionEntry::default()
        };
        let entries = vec![
            entry("greet", "void greet(String name)"),
            entry("greeting", "String greeting"),
        ];
        let _ = reply.send(self.answer(CompletionList {
            entries,
            complete: true,
        }));
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for FakeLanguage {
    fn start(
        &self,
        _: NavigationQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SourceSpan>>,
    ) {
        let target = self.definition.lock().unwrap().clone();
        let answer = match target {
            Some(target) => self.answer(vec![target]),
            None => Answer::new(Vec::new(), Confidence::Unknown, SOURCE),
        };
        let _ = reply.send(answer);
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for FakeLanguage {
    fn start(
        &self,
        _: DiagnosticsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self.diagnostics.lock().unwrap().clone();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<InlayHintQuery, Vec<InlayHintEntry>> for FakeLanguage {
    fn start(
        &self,
        query: InlayHintQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<InlayHintEntry>>,
    ) {
        let text = self.text_of(&query.uri).unwrap_or_default();
        let entries = self
            .inlays
            .lock()
            .unwrap()
            .iter()
            .map(|(offset, label, kind)| InlayHintEntry {
                position: point_in(&text, (*offset).min(text.len())),
                label: label.clone(),
                kind: *kind,
                padding_left: false,
                padding_right: true,
            })
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<CodeLensQuery, Vec<CodeLensEntry>> for FakeLanguage {
    fn start(
        &self,
        query: CodeLensQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<CodeLensEntry>>,
    ) {
        let text = self.text_of(&query.uri).unwrap_or_default();
        let entries = self
            .code_lens
            .lock()
            .unwrap()
            .iter()
            .map(|(offset, title, command)| {
                let offset = (*offset).min(text.len());
                let start = point_in(&text, offset);
                let end = point_in(&text, (offset + title.len()).min(text.len()));
                CodeLensEntry {
                    span: TextSpan { start, end },
                    title: title.clone(),
                    kind: lang_api::kind_of(title, Some(command.as_str())),
                    resolve: None,
                    command: Some(CodeLensCommand {
                        title: title.clone(),
                        command: command.clone(),
                        arguments: None,
                    }),
                }
            })
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<CodeLensResolveQuery, CodeLensEntry> for FakeLanguage {
    fn start(
        &self,
        query: CodeLensResolveQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<CodeLensEntry>,
    ) {
        let _ = reply.send(self.answer(query.lens));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for FakeLanguage {
    fn start(
        &self,
        query: ActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<ActionEntry>>,
    ) {
        let entries: Vec<ActionEntry> = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<DocumentSpot, Vec<SourceSpan>> for FakeLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<Vec<SourceSpan>>) {
        let spans = self.references.lock().unwrap().clone();
        let _ = reply.send(self.answer(spans));
    }
}

impl Provider<RenameQuery, WorkspaceChange> for FakeLanguage {
    fn start(
        &self,
        query: RenameQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<WorkspaceChange>,
    ) {
        let Some(text) = self.text_of(&query.spot.uri) else {
            let _ = reply.send(Answer::new(
                WorkspaceChange::default(),
                Confidence::Unknown,
                SOURCE,
            ));
            return;
        };
        let Some(word) = word_at(&text, query.spot.point) else {
            let _ = reply.send(self.answer(WorkspaceChange::default()));
            return;
        };
        let mut change = WorkspaceChange::default();
        change.edits.push(SourceEdit {
            uri: query.spot.uri.clone(),
            edits: replace_word(&text, &word, &query.new_name),
        });
        if let Some(path) = self.closed.lock().unwrap().clone()
            && let Ok(closed) = fs::read_to_string(&path)
        {
            change.edits.push(SourceEdit {
                uri: uri_of(&path),
                edits: replace_word(&closed, &word, &query.new_name),
            });
        }
        let _ = reply.send(self.answer(change));
    }
}

impl Provider<MoveQuery, Vec<MoveDestination>> for FakeLanguage {
    fn start(
        &self,
        _: MoveQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<MoveDestination>>,
    ) {
        let destinations = self.destinations.lock().unwrap().clone();
        let _ = reply.send(self.answer(destinations));
    }
}

impl Provider<MoveRequest, MoveOutcome> for FakeLanguage {
    fn start(
        &self,
        request: MoveRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<MoveOutcome>,
    ) {
        let Some(target) = self.move_target.lock().unwrap().clone() else {
            let _ = reply.send(Answer::new(
                MoveOutcome::default(),
                Confidence::Unknown,
                SOURCE,
            ));
            return;
        };
        let Some(text) = self.text_of(&request.spot.uri) else {
            let _ = reply.send(Answer::new(
                MoveOutcome::default(),
                Confidence::Unknown,
                SOURCE,
            ));
            return;
        };
        let first_line = text.lines().next().unwrap_or_default();
        let package = format!("package {};", request.destination.name);
        let edit = edit_in(&text, 0, first_line.len(), &package);
        let outcome = MoveOutcome {
            change: WorkspaceChange {
                edits: vec![SourceEdit {
                    uri: request.spot.uri.clone(),
                    edits: vec![edit],
                }],
                renames: vec![lang_api::FileRename {
                    from: request.spot.uri.clone(),
                    to: uri_of(&target),
                }],
            },
            error: None,
        };
        let _ = reply.send(self.answer(outcome));
    }
}

impl Provider<FormatQuery, Vec<DocumentEdit>> for FakeLanguage {
    fn start(
        &self,
        _: FormatQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<DocumentEdit>>,
    ) {
        let edit = DocumentEdit {
            span: TextSpan::default(),
            text: "// formatted\n".to_owned(),
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl LanguageSupport for FakeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn lang_api::SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }

    fn inlay_hints(&self) -> Option<&InlayHintProvider> {
        Some(self)
    }

    fn code_lens(&self) -> Option<&CodeLensProvider> {
        Some(self)
    }

    fn code_lens_resolve(&self) -> Option<&CodeLensResolveProvider> {
        Some(self)
    }

    fn format(&self) -> Option<&FormatProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn references(&self) -> Option<&ReferencesProvider> {
        Some(self)
    }

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self)
    }

    fn move_destinations(&self) -> Option<&MoveDestinationProvider> {
        Some(self)
    }

    fn move_file(&self) -> Option<&MoveProvider> {
        Some(self)
    }

    fn restart_server(&self) -> bool {
        self.restarts.fetch_add(1, Ordering::Relaxed);
        true
    }
}

pub(crate) fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    root
}

pub(crate) fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<FakeLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn click_editor(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
    modifiers: Modifiers,
) {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let x = bounds.left() + px(67. + column as f32 * 8.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(x, y), modifiers);
    cx.run_until_parked();
}

fn error(line: u32, start: u32, end: u32, message: &str) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line,
                character: start,
            },
            end: TextPoint {
                line,
                character: end,
            },
        },
        severity: Severity::Error,
        message: message.to_owned(),
        unnecessary: false,
    }
}

fn action(
    title: &str,
    kind: ActionKind,
    code: &str,
    uri: &str,
    edits: Vec<DocumentEdit>,
) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(kind),
        code: Some(code.to_owned()),
        preferred: true,
        edits: vec![SourceEdit {
            uri: uri.to_owned(),
            edits,
        }],
        renames: Vec::new(),
    }
}

#[ui_kit::gpui::test]
fn completion_from_typing_and_ctrl_space_inserts_the_entry(cx: &mut TestAppContext) {
    let root = project(
        "completion",
        &[("src/Type.txt", "\n"), ("src/Space.txt", "\n")],
    );
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let path = root.join("src").join("Type.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    type_text(cx, window, "g");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "typing a letter shows the list of the server"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "greet\n");

    let other = root.join("src").join("Space.txt");
    open_path(cx, window, &other);
    let other_view = editor_of(cx, window, &other);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "Ctrl+Space shows the list without a typed prefix"
    );
    press(cx, window, "down enter");
    assert_eq!(
        buffer_of(cx, &other_view),
        "greeting\n",
        "the second entry of the list is inserted"
    );
}

#[ui_kit::gpui::test]
fn definitions_open_in_another_tab_and_back_returns(cx: &mut TestAppContext) {
    let root = project(
        "navigation",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/Other.txt", "other()\ngreet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 1, 0);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other.clone()));
    let view = editor_of(cx, window, &other);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    press(cx, window, "ctrl-alt-left");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(main.clone()));
    let view = editor_of(cx, window, &main);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 0),
        "Back returns to the place the jump started from"
    );

    click_editor(
        cx,
        window,
        0,
        0,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    draw(cx, window);
    assert_eq!(
        active_path(cx, window),
        Some(other.clone()),
        "Ctrl+click goes to the definition"
    );
    let view = editor_of(cx, window, &other);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));

    press(cx, window, "ctrl-alt-left");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(main));
}

#[ui_kit::gpui::test]
fn diagnostics_reach_the_text_the_gutter_problems_and_f2(cx: &mut TestAppContext) {
    let root = project("diagnostics", &[("src/Main.txt", "one\ntwo\nthree\n")]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![
        error(1, 0, 3, "first error"),
        error(2, 0, 5, "second error"),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    draw(cx, window);
    let view = editor_of(cx, window, &main);

    let marks = view.read_with(cx, |view, _| view.diagnostic_marks());
    assert_eq!(marks.len(), 2, "the text holds every diagnostic");
    assert!(
        marks
            .iter()
            .any(|(severity, range)| *severity == Severity::Error
                && range.start == 4
                && range.end == 7),
        "the mark of the first error covers its word: {marks:?}"
    );

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    press(cx, window, "alt-6");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROBLEMS_TOOL))
            .unwrap(),
        "Alt+6 opens the Problems window"
    );
    let items = problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 2);
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(labels.contains(&"first error"), "{labels:?}");
    assert!(labels.contains(&"second error"), "{labels:?}");

    open_path(cx, window, &main);
    press(cx, window, "f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "F2 goes to the first error"
    );
    press(cx, window, "f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (2, 0),
        "F2 goes to the next error"
    );
    press(cx, window, "f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "F2 goes round after the last error"
    );
}

#[ui_kit::gpui::test]
fn alt_enter_lists_the_actions_and_applies_the_quick_fix(cx: &mut TestAppContext) {
    let root = project("quick-fix", &[("src/Fix.txt", "problem\n")]);
    let path = root.join("src").join("Fix.txt");
    let uri = uri_of(&path);
    let language = FakeLanguage::new();
    language.set_actions(vec![action(
        "Fix the problem",
        ActionKind::QuickFix,
        "quickfix.fix",
        &uri,
        vec![edit_in("problem\n", 0, 7, "fixed")],
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Alt+Enter shows the actions of the place"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "fixed\n");
}

#[ui_kit::gpui::test]
fn rename_previews_and_applies_edits_in_an_open_and_a_closed_file(cx: &mut TestAppContext) {
    let root = project(
        "rename",
        &[
            ("src/Open.txt", "alpha beta\n"),
            ("src/Closed.txt", "alpha gamma\n"),
        ],
    );
    let open = root.join("src").join("Open.txt");
    let closed = root.join("src").join("Closed.txt");
    let language = FakeLanguage::new();
    language.set_closed(closed.clone());
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "rename-popup").size.height > px(0.),
        "Shift+F6 asks for the new name"
    );
    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "delta");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "alpha beta\n",
        "the first Enter only previews"
    );
    assert!(
        bounds_of(cx, window, "rename-popup").size.height > px(0.),
        "the preview keeps the field open"
    );

    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "delta beta\n");
    assert_eq!(fs::read_to_string(&closed).unwrap(), "delta gamma\n");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("rename-popup").is_none(),
        "the second Enter applies the change and closes the field"
    );

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha beta\n",
        "Ctrl+Z takes the refactoring back in one step"
    );
}

#[ui_kit::gpui::test]
fn move_shows_packages_applies_the_edit_and_moves_the_tab(cx: &mut TestAppContext) {
    let root = project("move", &[("src/Main.txt", "package old;\nclass Main\n")]);
    let from = root.join("src").join("Main.txt");
    let to = root.join("src").join("newpkg").join("Main.txt");
    fs::create_dir_all(root.join("src").join("newpkg")).unwrap();
    let language = FakeLanguage::new();
    language.set_move(
        to.clone(),
        vec![MoveDestination {
            name: "newpkg".to_owned(),
            detail: Some("project  /project/src/main/source/newpkg".to_owned()),
            folder: Some(to.parent().unwrap().to_path_buf()),
            handle: "pkg".to_owned(),
        }],
    );
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &from);

    press(cx, window, "f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-popup").size.height > px(0.),
        "F6 lists the packages of the server"
    );
    type_text(cx, window, "new");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-popup").size.height > px(0.),
        "the package list is filtered by the typed text"
    );
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    assert!(!from.exists(), "the file left the old package");
    assert_eq!(
        fs::read_to_string(&to).unwrap(),
        "package newpkg;\nclass Main\n"
    );
    assert_eq!(active_path(cx, window), Some(to.clone()));
    let tabs = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs, vec![vec![to]]);
}

#[ui_kit::gpui::test]
fn refactoring_keys_apply_their_edits_and_undo_in_one_step(cx: &mut TestAppContext) {
    let root = project("refactor", &[("src/Refactor.txt", "value\n")]);
    let path = root.join("src").join("Refactor.txt");
    let uri = uri_of(&path);
    let language = FakeLanguage::new();
    language.set_actions(vec![
        action(
            "Extract method",
            ActionKind::Refactor,
            "refactor.extract.function",
            &uri,
            vec![edit_in("value\n", 0, 5, "method()")],
        ),
        action(
            "Extract variable",
            ActionKind::Refactor,
            "refactor.extract.variable",
            &uri,
            vec![edit_in("value\n", 0, 5, "variable")],
        ),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-m");
    assert_eq!(buffer_of(cx, &view), "method()\n");
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "value\n");

    press(cx, window, "ctrl-alt-v");
    assert_eq!(buffer_of(cx, &view), "variable\n");
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "value\n");

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Refactor This lists the refactorings"
    );
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "method()\n");
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "value\n");
}

#[ui_kit::gpui::test]
fn format_and_optimize_imports_apply_the_edits_of_the_server(cx: &mut TestAppContext) {
    let root = project("format", &[("src/Format.txt", "value\n")]);
    let path = root.join("src").join("Format.txt");
    let uri = uri_of(&path);
    let language = FakeLanguage::new();
    language.set_actions(vec![action(
        "Organize Imports",
        ActionKind::OrganizeImports,
        "source.organizeImports",
        &uri,
        vec![edit_in("value\n", 0, 5, "import a;")],
    )]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-l");
    assert_eq!(buffer_of(cx, &view), "// formatted\nvalue\n");
    press(cx, window, "ctrl-z");

    press(cx, window, "ctrl-alt-o");
    assert_eq!(buffer_of(cx, &view), "import a;\n");
}

#[ui_kit::gpui::test]
fn find_usages_lists_the_places_and_enter_opens_one(cx: &mut TestAppContext) {
    let root = project(
        "usages",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&main),
            span: span_in("greet\n", 0, 5),
        },
        SourceSpan {
            uri: uri_of(&other),
            span: span_in("greet\n", 0, 5),
        },
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "alt-f7");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Alt+F7 opens the Usages window"
    );
    press(cx, window, "alt-3");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Alt+3 closes the Usages window"
    );
    press(cx, window, "alt-3");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Alt+3 opens the Usages window again"
    );
    let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    let items = usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 2);
    let expected = items[0].clone();

    press(cx, window, "down enter");
    assert_eq!(active_path(cx, window), Some(expected.path.clone()));
    let view = editor_of(cx, window, &expected.path);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (expected.line, expected.column)
    );
}

#[ui_kit::gpui::test]
fn restart_language_server_from_find_action_starts_it_again(cx: &mut TestAppContext) {
    let root = project("restart", &[("src/Main.txt", "value\n")]);
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);

    open_palette(cx, window, keys::FIND_ACTION);
    cx.simulate_keystrokes(window.into(), "r e s t a r t");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "enter");
    cx.run_until_parked();

    assert_eq!(language.restarts(), 1);
}
