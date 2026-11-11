//! The tags query against the grammar.

use lang_api::SymbolKind;

const SAMPLE: &str = r#"package com.example.shop;

public class Owner {
    private static final int LIMIT = 10;
    private String name, city;

    public Owner(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }

    static class Pet {
        void feed() {}
    }

    enum Kind { CAT, DOG }

    record Visit(String date) {}

    interface Listener {
        void changed();
    }
}
"#;

#[test]
fn declarations_come_with_kind_container_and_depth() {
    let symbols = java_syntax::symbols().symbols(SAMPLE);
    let found: Vec<(String, SymbolKind, u32)> = symbols
        .iter()
        .map(|symbol| (symbol.qualified_name(), symbol.kind, symbol.depth))
        .collect();

    let expected = [
        ("com.example.shop", SymbolKind::Module, 0),
        ("com.example.shop.Owner", SymbolKind::Class, 0),
        ("com.example.shop.Owner.LIMIT", SymbolKind::Field, 1),
        ("com.example.shop.Owner.name", SymbolKind::Field, 1),
        ("com.example.shop.Owner.city", SymbolKind::Field, 1),
        ("com.example.shop.Owner.Owner", SymbolKind::Constructor, 1),
        ("com.example.shop.Owner.getName", SymbolKind::Method, 1),
        ("com.example.shop.Owner.Pet", SymbolKind::Class, 1),
        ("com.example.shop.Owner.Pet.feed", SymbolKind::Method, 2),
        ("com.example.shop.Owner.Kind", SymbolKind::Enum, 1),
        ("com.example.shop.Owner.Kind.CAT", SymbolKind::Constant, 2),
        ("com.example.shop.Owner.Kind.DOG", SymbolKind::Constant, 2),
        ("com.example.shop.Owner.Visit", SymbolKind::Class, 1),
        ("com.example.shop.Owner.Listener", SymbolKind::Interface, 1),
        (
            "com.example.shop.Owner.Listener.changed",
            SymbolKind::Method,
            2,
        ),
    ];
    let expected: Vec<_> = expected
        .iter()
        .map(|(name, kind, depth)| (name.to_string(), *kind, *depth))
        .collect();
    assert_eq!(found, expected);

    let owner = &symbols[1];
    assert_eq!(&SAMPLE[owner.name_range.clone()], "Owner");
    assert!(SAMPLE[owner.range.clone()].starts_with("public class Owner"));
}

#[test]
fn broken_code_still_gives_what_parses() {
    let symbols = java_syntax::symbols().symbols("class A { void run( { } class B {}");
    assert!(symbols.iter().any(|symbol| symbol.name == "A"));
}
