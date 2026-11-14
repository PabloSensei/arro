//! The declarations of the open files, read off the UI thread, for the breadcrumbs, the
//! sticky lines and the Structure tool window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use editor_view::{EditorView, LineDeclaration, OutlineItem, OutlineKind, Semantic};
use futures_util::StreamExt;
use lang_api::{FileSymbol, FileSymbolsQuery, Symbol, SymbolKind, SymbolProvider, ask, offset_in};
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
            kind: kind_of(symbol.kind),
            depth: symbol.depth,
            range: symbol.range.clone(),
            name_range: symbol.name_range.clone(),
        })
        .collect()
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
        if found.is_empty()
            && let Some(provider) = native
        {
            let (items, lines) = cx
                .background_executor()
                .spawn(async move {
                    let symbols = provider.file_symbols(&path, &text);
                    (items_of(&symbols), declarations_of(&symbols, &text))
                })
                .await;
            found = items;
            declarations = lines;
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
    use lang_api::{FileSymbol, Symbol, SymbolKind, TextPoint, TextSpan};

    use super::{icon_of, items_of, signature_of};

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
}
