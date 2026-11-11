//! Search Everywhere: classes, files, symbols and actions behind one field.

use std::path::{Path, PathBuf};

use crate::files::rank_files;
use crate::fuzzy::{FuzzyMatcher, NameMatcher, Ranked, rank};
use crate::symbols::{SymbolIndex, SymbolMatch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    All,
    Classes,
    Files,
    Symbols,
    Actions,
}

/// An action as Find Action shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionItem {
    pub id: &'static str,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    File(PathBuf),
    Symbol(SymbolMatch),
    Action(&'static str),
}

/// One section of the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub tab: Tab,
    pub items: Vec<Ranked<Found>>,
}

pub struct Sources<'a> {
    pub root: &'a Path,
    pub files: &'a [PathBuf],
    pub symbols: &'a SymbolIndex,
    pub actions: &'a [ActionItem],
}

/// Groups in the order of the All tab, empty ones left out. `limit` applies per group.
pub fn search_everywhere(pattern: &str, tab: Tab, sources: &Sources, limit: usize) -> Vec<Group> {
    if pattern.trim().is_empty() {
        return Vec::new();
    }
    let tabs: &[Tab] = match tab {
        Tab::All => &[Tab::Classes, Tab::Files, Tab::Symbols, Tab::Actions],
        Tab::Classes => &[Tab::Classes],
        Tab::Files => &[Tab::Files],
        Tab::Symbols => &[Tab::Symbols],
        Tab::Actions => &[Tab::Actions],
    };
    tabs.iter()
        .map(|&tab| Group {
            tab,
            items: search_tab(pattern, tab, sources, limit),
        })
        .filter(|group| !group.items.is_empty())
        .collect()
}

fn search_tab(pattern: &str, tab: Tab, sources: &Sources, limit: usize) -> Vec<Ranked<Found>> {
    let symbols = |wanted: fn(&lang_api::Symbol) -> bool| {
        sources
            .symbols
            .search(pattern, wanted, limit)
            .into_iter()
            .map(|ranked| Ranked {
                item: Found::Symbol(ranked.item),
                score: ranked.score,
            })
            .collect()
    };
    match tab {
        Tab::All => Vec::new(),
        Tab::Classes => symbols(|symbol| symbol.kind.is_type()),
        Tab::Symbols => symbols(|symbol| !symbol.kind.is_type()),
        Tab::Files => {
            let files = sources.files.iter().map(PathBuf::as_path);
            rank_files(pattern, sources.root, files, limit)
                .into_iter()
                .map(|ranked| Ranked {
                    item: Found::File(ranked.item),
                    score: ranked.score,
                })
                .collect()
        }
        Tab::Actions => {
            let mut matcher = NameMatcher::new();
            let matcher: &mut dyn FuzzyMatcher = &mut matcher;
            let actions = sources.actions.iter();
            rank(matcher, pattern, actions, |action| &action.title, limit)
                .into_iter()
                .map(|ranked| Ranked {
                    item: Found::Action(ranked.item.id),
                    score: ranked.score,
                })
                .collect()
        }
    }
}
