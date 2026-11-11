//! The declarations of the open files, read off the UI thread, for the breadcrumbs, the
//! sticky lines and the Structure tool window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use editor_view::{EditorView, OutlineItem, OutlineKind, Semantic};
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
        if let (Some(support), Some(uri)) = (server, uri)
            && let Some(provider) = support.file_symbols()
        {
            let mut replies = ask(provider, FileSymbolsQuery(uri));
            if let Some(answer) = replies.next().await
                && !answer.value.is_empty()
            {
                found = items_of_server(&answer.value, &text);
            }
        }
        if found.is_empty()
            && let Some(provider) = native
        {
            found = cx
                .background_executor()
                .spawn(async move { items_of(&provider.file_symbols(&path, &text)) })
                .await;
        }
        weak.update(cx, |view, cx| {
            if view.outline_serial() == serial {
                view.set_outline(found, cx);
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
}
