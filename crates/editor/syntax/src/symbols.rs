//! Declarations read with a tags query: `@name` on the name, `@definition.<kind>` on the
//! whole declaration. The kinds are the neutral ones of `lang-api`.

use std::ops::Range;
use std::path::Path;

use lang_api::{Symbol, SymbolKind, SymbolProvider};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

const NAME: &str = "name";
const DEFINITION: &str = "definition.";

fn kind_of(capture: &str) -> Option<SymbolKind> {
    let kind = match capture.strip_prefix(DEFINITION)? {
        "module" => SymbolKind::Module,
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "constructor" => SymbolKind::Constructor,
        "field" => SymbolKind::Field,
        "constant" => SymbolKind::Constant,
        _ => return None,
    };
    Some(kind)
}

/// A [`SymbolProvider`] for any grammar with a tags query.
pub struct TagSymbols {
    language: Language,
    query: Query,
}

impl TagSymbols {
    /// Fails on a query that does not fit the grammar.
    pub fn new(language: Language, query: &str) -> Result<Self, tree_sitter::QueryError> {
        let query = Query::new(&language, query)?;
        Ok(Self { language, query })
    }

    pub fn symbols(&self, text: &str) -> Vec<Symbol> {
        let mut parser = Parser::new();
        if parser.set_language(&self.language).is_err() {
            return Vec::new();
        }
        let Some(tree) = parser.parse(text, None) else {
            return Vec::new();
        };
        let names = self.query.capture_names();
        let mut found = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), text.as_bytes());
        while let Some(item) = matches.next() {
            let mut name = None;
            let mut declaration = None;
            for capture in item.captures() {
                let capture_name = names[capture.index as usize];
                if capture_name == NAME {
                    name = Some(capture.node.byte_range());
                } else if let Some(kind) = kind_of(capture_name) {
                    declaration = Some((kind, capture.node.byte_range()));
                }
            }
            if let (Some(name_range), Some((kind, range))) = (name, declaration) {
                found.push(Symbol {
                    name: text[name_range.clone()].to_owned(),
                    kind,
                    container: None,
                    depth: 0,
                    range,
                    name_range,
                });
            }
        }
        nest(found)
    }
}

/// Sorts by position and fills `container` and `depth` from the ranges. A module, such as
/// a package line, contains what follows it in the file.
fn nest(mut symbols: Vec<Symbol>) -> Vec<Symbol> {
    symbols.sort_by_key(|symbol| (symbol.range.start, std::cmp::Reverse(symbol.range.end)));
    symbols.dedup_by(|next, kept| next.name_range == kept.name_range);
    let module = symbols
        .iter()
        .find(|symbol| symbol.kind == SymbolKind::Module)
        .map(|symbol| symbol.name.clone());
    // Enclosing declarations: range and qualified name. `int a, b;` gives two symbols
    // with one range, which are neighbours.
    let mut open: Vec<(Range<usize>, String)> = Vec::new();
    for symbol in &mut symbols {
        if symbol.kind == SymbolKind::Module {
            continue;
        }
        while open
            .last()
            .is_some_and(|(range, _)| range.end <= symbol.range.start || *range == symbol.range)
        {
            open.pop();
        }
        symbol.depth = open.len() as u32;
        symbol.container = open.last().map(|(_, name)| name.clone()).or(module.clone());
        open.push((symbol.range.clone(), symbol.qualified_name()));
    }
    symbols
}

impl SymbolProvider for TagSymbols {
    fn file_symbols(&self, _path: &Path, text: &str) -> Vec<Symbol> {
        self.symbols(text)
    }
}
