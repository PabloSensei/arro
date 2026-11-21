//! The declarations of the open files, read off the UI thread, for the breadcrumbs, the
//! sticky lines and the Structure tool window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use editor_view::{EditorView, LineDeclaration, OutlineItem, OutlineKind, Semantic};
use futures_util::StreamExt;
use lang_api::{
    Declared, FileSymbol, FileSymbolsQuery, GraphVisibility, Symbol, SymbolKind, SymbolProvider,
    ask, offset_in,
};
use ui_kit::*;

/// How long typing has to pause before the declarations are read again.
const DELAY: Duration = Duration::from_millis(250);

/// How the language of a path lists the declarations of a file.
pub struct SymbolsFor(pub fn(&Path) -> Option<&'static dyn SymbolProvider>);

impl Global for SymbolsFor {}

/// The editors that are open, by path.
#[derive(Default)]
pub struct OpenEditors(HashMap<PathBuf, WeakEntity<EditorView>>);

impl Global for OpenEditors {}

impl OpenEditors {
    pub fn add(cx: &mut App, path: PathBuf, view: WeakEntity<EditorView>) {
        cx.default_global::<Self>().0.insert(path, view);
    }

    pub fn get(cx: &App, path: &Path) -> Option<Entity<EditorView>> {
        cx.try_global::<Self>()?.0.get(path)?.upgrade()
    }

    /// The open editors, for a model that has to reach every one of them.
    pub fn all(cx: &App) -> Vec<Entity<EditorView>> {
        let Some(editors) = cx.try_global::<Self>() else {
            return Vec::new();
        };
        editors
            .0
            .values()
            .filter_map(|view| view.upgrade())
            .collect()
    }

    /// An editor of `path` holds edits that are not on disk.
    pub fn is_dirty(path: &Path, cx: &App) -> bool {
        let editor = Self::get(cx, path);
        editor.is_some_and(|editor| editor.read(cx).is_dirty())
    }

    /// A clean editor of `path` takes the disk text as one edit that undo can take back.
    pub fn reload(path: &Path, cx: &mut App) {
        if let Some(editor) = Self::get(cx, path) {
            editor.update(cx, |editor, cx| editor.reload(cx));
        }
    }

    /// The lines of the primary selection, from one, and its text.
    pub fn selection(path: &Path, cx: &App) -> Option<(usize, usize, String)> {
        let editor = Self::get(cx, path)?;
        let editor = editor.read(cx);
        let text = editor.text()?;
        let (range, _) = editor.selection()?;
        let picked = text.get(range.clone())?;
        if picked.is_empty() {
            return None;
        }
        let line_at = |offset: usize| text[..offset].matches('\n').count() + 1;
        let first = line_at(range.start);
        let last = line_at(range.end.saturating_sub(1).max(range.start));
        Some((first, last, picked.to_string()))
    }

    /// The text of the editor of `path` with its unsaved edits.
    pub fn text(path: &Path, cx: &App) -> Option<String> {
        Self::get(cx, path)?.read(cx).text()
    }
}

fn kind_of(kind: SymbolKind) -> OutlineKind {
    match kind {
        SymbolKind::Module => OutlineKind::Module,
        SymbolKind::Class => OutlineKind::Class,
        SymbolKind::Interface => OutlineKind::Interface,
        SymbolKind::Enum => OutlineKind::Enum,
        SymbolKind::Function => OutlineKind::Function,
        SymbolKind::Method => OutlineKind::Method,
        SymbolKind::Constructor => OutlineKind::Constructor,
        SymbolKind::Field => OutlineKind::Field,
        SymbolKind::Constant => OutlineKind::Constant,
    }
}

/// The icon of a declaration in the Structure tool window.
pub fn icon_of(kind: OutlineKind) -> &'static str {
    match kind {
        OutlineKind::Module => "nodes/package",
        OutlineKind::Class => "nodes/class",
        OutlineKind::Interface => "nodes/interface",
        OutlineKind::Enum => "nodes/enum",
        OutlineKind::Function | OutlineKind::Method | OutlineKind::Constructor => "nodes/method",
        OutlineKind::Field | OutlineKind::Other => "nodes/field",
        OutlineKind::Constant => "nodes/constant",
    }
}

pub fn items_of(symbols: &[Symbol]) -> Vec<OutlineItem> {
    symbols
        .iter()
        .map(|symbol| OutlineItem {
            name: symbol.name.clone(),
            signature: None,
            visibility: None,
            kind: kind_of(symbol.kind),
            depth: symbol.depth,
            range: symbol.range.clone(),
            name_range: symbol.name_range.clone(),
        })
        .collect()
}

/// The declarations of a file with the signatures and the visibility its language gives.
pub fn items_of_declared(declared: &[Declared]) -> Vec<OutlineItem> {
    declared
        .iter()
        .map(|found| OutlineItem {
            name: found.symbol.name.clone(),
            signature: found.signature.clone(),
            visibility: found.visibility,
            kind: kind_of(found.symbol.kind),
            depth: found.symbol.depth,
            range: found.symbol.range.clone(),
            name_range: found.symbol.name_range.clone(),
        })
        .collect()
}

/// Adds to the items of the server what the native pass knows and the server does not: the
/// visibility, and the signature where the server sent none. Declarations meet by the offset
/// of their name.
pub fn complete_from(items: &mut [OutlineItem], declared: &[Declared]) {
    let by_name: HashMap<usize, &Declared> = declared
        .iter()
        .map(|found| (found.symbol.name_range.start, found))
        .collect();
    for item in items {
        let Some(found) = by_name.get(&item.name_range.start) else {
            continue;
        };
        item.visibility = found.visibility;
        if item.signature.is_none() {
            item.signature.clone_from(&found.signature);
        }
    }
}

/// The mark of a visibility that stands beside the icon of a declaration; `""` when the
/// declaration has none. A lock is private, a circle public, a diamond protected and a ring
/// the default of a package.
pub fn visibility_icon(visibility: Option<GraphVisibility>) -> &'static str {
    match visibility {
        Some(GraphVisibility::Public) => "nodes/c_public",
        Some(GraphVisibility::Protected) => "nodes/c_protected",
        Some(GraphVisibility::Package) => "nodes/c_plocal",
        Some(GraphVisibility::Private) => "general/lock",
        None => "",
    }
}

/// The line of a byte offset in `text`, from zero.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
}

/// The declarations whose names start on a line, from the symbols of the language.
pub fn declarations_of(symbols: &[Symbol], text: &str) -> Vec<LineDeclaration> {
    symbols
        .iter()
        .map(|symbol| LineDeclaration {
            line: line_of(text, symbol.name_range.start),
            name: symbol.name.clone(),
            owner: symbol.container.clone(),
            kind: kind_of(symbol.kind),
        })
        .collect()
}

/// The qualified name of a type below `owner`, with `$` between nested types the way
/// some adapters name them.
fn type_name(owner: Option<&str>, owner_is_type: bool, name: &str) -> String {
    match owner.filter(|owner| !owner.is_empty()) {
        Some(owner) if owner_is_type => format!("{owner}${name}"),
        Some(owner) => format!("{owner}.{name}"),
        None => name.to_owned(),
    }
}

fn server_declarations(
    items: &[FileSymbol],
    text: &str,
    owner: Option<&str>,
    owner_is_type: bool,
    found: &mut Vec<LineDeclaration>,
) {
    for symbol in items {
        if symbol.kind == SymbolKind::Module {
            server_declarations(&symbol.children, text, Some(&symbol.name), false, found);
            continue;
        }
        found.push(LineDeclaration {
            line: line_of(text, offset_in(text, symbol.name_range.start)),
            name: symbol.name.clone(),
            owner: owner.map(str::to_owned),
            kind: kind_of(symbol.kind),
        });
        if symbol.kind.is_type() {
            let qualified = type_name(owner, owner_is_type, &symbol.name);
            server_declarations(&symbol.children, text, Some(&qualified), true, found);
        }
    }
}

/// The declarations the server sends, with the type that owns each one.
pub fn declarations_of_server(items: &[FileSymbol], text: &str) -> Vec<LineDeclaration> {
    // A module often stands next to the declarations of the file rather than around
    // them: it names the place where all of them live.
    let module = items
        .iter()
        .find(|symbol| symbol.kind == SymbolKind::Module)
        .map(|symbol| symbol.name.as_str());
    let mut found = Vec::new();
    server_declarations(items, text, module, false, &mut found);
    found
}

/// `processFindForm(int, Owner): String` and `owners: OwnerRepository`: the name of the
/// declaration and the detail the server gives after it.
fn signature_of(symbol: &FileSymbol) -> Option<String> {
    let detail = symbol.detail.as_deref()?.trim();
    let detail = detail.strip_prefix(':').unwrap_or(detail).trim();
    if detail.is_empty() {
        return None;
    }
    let name = symbol.name.trim();
    // Parameters and type arguments follow the name without a colon.
    Some(if detail.starts_with(['(', '<', '[']) {
        format!("{name}{}", detail.replace(" : ", ": "))
    } else {
        format!("{name}: {detail}")
    })
}

fn server_items(items: &[FileSymbol], text: &str, depth: u32, found: &mut Vec<OutlineItem>) {
    for symbol in items {
        // The package is not a declaration of the file, its types start at the top.
        if symbol.kind == SymbolKind::Module {
            server_items(&symbol.children, text, depth, found);
            continue;
        }
        found.push(OutlineItem {
            name: symbol.name.clone(),
            signature: signature_of(symbol),
            visibility: None,
            kind: kind_of(symbol.kind),
            depth,
            range: offset_in(text, symbol.range.start)..offset_in(text, symbol.range.end),
            name_range: offset_in(text, symbol.name_range.start)
                ..offset_in(text, symbol.name_range.end),
        });
        server_items(&symbol.children, text, depth + 1, found);
    }
}

/// The declarations the server sends, ready for the outline.
pub fn items_of_server(items: &[FileSymbol], text: &str) -> Vec<OutlineItem> {
    let mut found = Vec::new();
    server_items(items, text, 0, &mut found);
    found
}

/// Reads the declarations of the file again. A change waits for a pause in typing, an
/// answer that a newer request overtook is dropped.
pub fn refresh(view: &Entity<EditorView>, wait: bool, cx: &mut App) {
    let path = view.read(cx).path().to_path_buf();
    let native = cx
        .try_global::<SymbolsFor>()
        .and_then(|symbols| (symbols.0)(&path));
    let server = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
        .filter(|support| support.file_symbols().is_some());
    if native.is_none() && server.is_none() {
        return;
    }
    let uri = ::platform::CanonPath::new(&path)
        .ok()
        .map(|path| path.to_uri());
    let serial = view.update(cx, |view, _| view.next_outline_serial());
    let weak = view.downgrade();
    cx.spawn(async move |cx| {
        if wait {
            cx.background_executor().timer(DELAY).await;
        }
        let request = weak
            .read_with(cx, |view, _| {
                (view.outline_serial() == serial).then(|| (view.path().to_path_buf(), view.text()))
            })
            .ok()
            .flatten();
        let Some((path, Some(text))) = request else {
            return;
        };
        let mut found = Vec::new();
        let mut declarations = Vec::new();
        if let (Some(support), Some(uri)) = (server, uri)
            && let Some(provider) = support.file_symbols()
        {
            let mut replies = ask(provider, FileSymbolsQuery(uri));
            if let Some(answer) = replies.next().await
                && !answer.value.is_empty()
            {
                found = items_of_server(&answer.value, &text);
                declarations = declarations_of_server(&answer.value, &text);
            }
        }
        // The native pass runs even when the server answers: the server sends no visibility,
        // and a server that sends no signature gets the one of the language.
        let (declared, lines) = match native {
            Some(provider) => {
                cx.background_executor()
                    .spawn(async move {
                        let declared = provider.file_declarations(&path, &text);
                        let symbols: Vec<Symbol> =
                            declared.iter().map(|found| found.symbol.clone()).collect();
                        let lines = declarations_of(&symbols, &text);
                        (declared, lines)
                    })
                    .await
            }
            None => (Vec::new(), Vec::new()),
        };
        if found.is_empty() {
            found = items_of_declared(&declared);
            declarations = lines;
        } else {
            complete_from(&mut found, &declared);
        }
        weak.update(cx, |view, cx| {
            if view.outline_serial() == serial {
                view.set_outline(found, cx);
                view.set_line_declarations(declarations, cx);
            }
        })
        .ok();
    })
    .detach();
}

#[cfg(test)]
mod checks {
    use editor_view::{OutlineItem, OutlineKind};
    use lang_api::{
        Declared, FileSymbol, GraphVisibility, Symbol, SymbolKind, TextPoint, TextSpan,
    };

    use super::{complete_from, icon_of, items_of, signature_of, visibility_icon};

    fn symbol(name: &str, detail: Option<&str>) -> FileSymbol {
        let span = TextSpan {
            start: TextPoint {
                line: 0,
                character: 0,
            },
            end: TextPoint {
                line: 0,
                character: 1,
            },
        };
        FileSymbol {
            name: name.to_owned(),
            detail: detail.map(str::to_owned),
            kind: SymbolKind::Method,
            range: span,
            name_range: span,
            children: Vec::new(),
        }
    }

    #[test]
    fn a_signature_has_the_parameters_next_to_the_name() {
        assert_eq!(
            signature_of(&symbol("processFindForm", Some("(int, Owner) : String"))),
            Some("processFindForm(int, Owner): String".to_owned())
        );
        assert_eq!(
            signature_of(&symbol("owners", Some(": OwnerRepository"))),
            Some("owners: OwnerRepository".to_owned())
        );
        assert_eq!(
            signature_of(&symbol("process(int)", Some(": String"))),
            Some("process(int): String".to_owned())
        );
        assert_eq!(signature_of(&symbol("run", None)), None);
    }

    #[test]
    fn symbols_keep_their_places_and_depth() {
        let symbols = [Symbol {
            name: "run".into(),
            kind: SymbolKind::Method,
            container: Some("Main".into()),
            depth: 1,
            range: 10..40,
            name_range: 14..17,
        }];
        let items = items_of(&symbols);
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].depth, items[0].range.clone()), (1, 10..40));
        assert!(items[0].kind.is_block());
        assert_eq!(icon_of(items[0].kind), "nodes/method");
    }

    #[test]
    fn a_declaration_of_a_line_keeps_the_type_that_owns_it() {
        let text = "package demo;\n\nclass Probe {\n    int value;\n    void work() {}\n}\n";
        let at = text.find("work").unwrap();
        let symbols = [Symbol {
            name: "work".into(),
            kind: SymbolKind::Method,
            container: Some("demo.Probe".into()),
            depth: 1,
            range: 0..text.len(),
            name_range: at..at + 4,
        }];
        let declarations = super::declarations_of(&symbols, text);
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].line, 4);
        assert_eq!(declarations[0].owner.as_deref(), Some("demo.Probe"));
        assert_eq!(declarations[0].kind, editor_view::OutlineKind::Method);
    }

    fn file_symbol(
        text: &str,
        name: &str,
        kind: SymbolKind,
        name_at: usize,
        children: Vec<FileSymbol>,
    ) -> FileSymbol {
        let line = text[..name_at].matches('\n').count();
        let line_start = text[..name_at].rfind('\n').map_or(0, |index| index + 1);
        FileSymbol {
            name: name.into(),
            detail: None,
            kind,
            range: TextSpan {
                start: TextPoint {
                    line: 0,
                    character: 0,
                },
                end: TextPoint {
                    line: 6,
                    character: 0,
                },
            },
            name_range: TextSpan {
                start: TextPoint {
                    line: line as u32,
                    character: (name_at - line_start) as u32,
                },
                end: TextPoint {
                    line: line as u32,
                    character: (name_at - line_start + name.len()) as u32,
                },
            },
            children,
        }
    }

    #[test]
    fn a_server_declaration_names_the_package_and_the_nested_types() {
        let text = "package demo;\n\nclass Probe {\n    void work() {}\n    class Inner {\n        int field;\n    }\n}\n";
        let items = vec![file_symbol(
            text,
            "demo",
            SymbolKind::Module,
            text.find("demo").unwrap(),
            vec![file_symbol(
                text,
                "Probe",
                SymbolKind::Class,
                text.find("Probe").unwrap(),
                vec![
                    file_symbol(
                        text,
                        "work",
                        SymbolKind::Method,
                        text.find("work").unwrap(),
                        vec![],
                    ),
                    file_symbol(
                        text,
                        "Inner",
                        SymbolKind::Class,
                        text.find("Inner").unwrap(),
                        vec![file_symbol(
                            text,
                            "field",
                            SymbolKind::Field,
                            text.find("field").unwrap(),
                            vec![],
                        )],
                    ),
                ],
            )],
        )];
        let declarations = super::declarations_of_server(&items, text);
        let work = declarations
            .iter()
            .find(|declaration| declaration.name == "work")
            .expect("the method");
        assert_eq!(work.owner.as_deref(), Some("demo.Probe"));
        assert_eq!(work.line, 3);
        let field = declarations
            .iter()
            .find(|declaration| declaration.name == "field")
            .expect("the field of the nested class");
        assert_eq!(field.owner.as_deref(), Some("demo.Probe$Inner"));
        assert_eq!(field.line, 5);
    }

    fn native_declaration(
        name: &str,
        at: usize,
        signature: Option<&str>,
        visibility: Option<GraphVisibility>,
    ) -> Declared {
        Declared {
            symbol: Symbol {
                name: name.into(),
                kind: SymbolKind::Method,
                container: None,
                depth: 0,
                range: at..at + 40,
                name_range: at..at + name.len(),
            },
            signature: signature.map(str::to_owned),
            visibility,
        }
    }

    fn server_item(name: &str, at: usize, signature: Option<&str>) -> OutlineItem {
        OutlineItem {
            name: name.into(),
            signature: signature.map(str::to_owned),
            visibility: None,
            kind: OutlineKind::Method,
            depth: 0,
            range: at..at + 40,
            name_range: at..at + name.len(),
        }
    }

    #[test]
    fn a_server_item_takes_the_visibility_and_a_signature_the_server_missed() {
        let declared = [
            native_declaration(
                "getPet",
                20,
                Some("getPet(String): Pet"),
                Some(GraphVisibility::Public),
            ),
            native_declaration(
                "count",
                80,
                Some("count: int"),
                Some(GraphVisibility::Private),
            ),
        ];
        let mut items = vec![
            server_item("getPet", 20, None),
            server_item("count", 80, Some("count: long")),
            server_item("other", 140, None),
        ];
        complete_from(&mut items, &declared);
        assert_eq!(
            items[0].signature.as_deref(),
            Some("getPet(String): Pet"),
            "the signature the server did not send comes from the language"
        );
        assert_eq!(items[0].visibility, Some(GraphVisibility::Public));
        assert_eq!(
            items[1].signature.as_deref(),
            Some("count: long"),
            "the server keeps the signature it sent"
        );
        assert_eq!(items[1].visibility, Some(GraphVisibility::Private));
        assert_eq!(
            items[2].visibility, None,
            "a declaration the language did not read has no visibility"
        );
    }

    #[test]
    fn each_visibility_has_a_mark_and_none_has_no_mark() {
        assert_eq!(
            visibility_icon(Some(GraphVisibility::Public)),
            "nodes/c_public"
        );
        assert_eq!(
            visibility_icon(Some(GraphVisibility::Protected)),
            "nodes/c_protected"
        );
        assert_eq!(
            visibility_icon(Some(GraphVisibility::Package)),
            "nodes/c_plocal"
        );
        assert_eq!(
            visibility_icon(Some(GraphVisibility::Private)),
            "general/lock"
        );
        assert_eq!(visibility_icon(None), "");
    }
}
