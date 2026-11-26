//! The built-in themes as data: every token is there, the text reads on every background
//! WCAG asks 4.5:1 for and 7:1 for High Contrast, and the themes of the settings folder
//! join the library around the broken files.

use theme::{Color, ThemeFile, ThemeLibrary, Tokens, slug, themes_folder};

/// WCAG 2 relative luminance of an opaque colour.
fn luminance(color: Color) -> f32 {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The pairs the window writes text and code on, with the least ratio WCAG asks.
fn checks(theme: &Tokens<Color>) -> Vec<(&'static str, Color, Color, f32)> {
    let code_backgrounds = [theme.editor_bg, theme.caret_row, theme.execution_line];
    let mut checks = vec![
        ("text on island", theme.text, theme.island_bg, 4.5),
        ("text on main", theme.text, theme.main_bg, 4.5),
        ("text on popup", theme.text, theme.popup_bg, 4.5),
        ("link on island", theme.link, theme.island_bg, 4.5),
        ("link on main", theme.link, theme.main_bg, 4.5),
        (
            "over_accent on accent",
            theme.over_accent,
            theme.accent,
            4.5,
        ),
        ("match_text on match", theme.match_text, theme.match_bg, 4.5),
        ("fold_text on fold", theme.fold_text, theme.fold_bg, 4.5),
        ("text on selection", theme.text, theme.selection, 4.5),
        ("text on tab selected", theme.text, theme.tab_selected, 4.5),
        ("text on feedback", theme.text, theme.feedback_bg, 4.5),
    ];
    for (name, color) in [
        ("code", theme.code),
        ("keyword", theme.keyword),
        ("string", theme.string),
        ("number", theme.number),
        ("doc_tag", theme.doc_tag),
        ("function", theme.function),
        ("annotation", theme.annotation),
        ("field", theme.field),
    ] {
        for background in code_backgrounds {
            checks.push((name, color, background, 4.5));
        }
    }
    for (name, color) in [
        ("code", theme.code),
        ("keyword", theme.keyword),
        ("string", theme.string),
        ("number", theme.number),
        ("function", theme.function),
        ("field", theme.field),
    ] {
        checks.push((name, color, theme.selection, 4.5));
    }
    for (name, color, background) in [
        ("text_muted", theme.text_muted, theme.island_bg),
        ("text_secondary", theme.text_secondary, theme.main_bg),
        ("comment", theme.comment, theme.editor_bg),
        ("doc", theme.doc, theme.editor_bg),
        ("unused", theme.unused, theme.editor_bg),
        ("line_number", theme.line_number, theme.editor_bg),
        ("inlay", theme.inlay, theme.editor_bg),
        // The values of the stop line and its arrow stand on the tint of the line.
        (
            "inlay on the execution line",
            theme.inlay,
            theme.execution_line,
        ),
        (
            "accent on the execution line",
            theme.accent,
            theme.execution_line,
        ),
    ] {
        checks.push((name, color, background, 3.));
    }
    checks
}

#[test]
fn every_built_in_theme_names_every_token() {
    let library = ThemeLibrary::builtin();
    assert!(
        library.errors().is_empty(),
        "the built-in themes are whole: {:?}",
        library.errors()
    );
    for id in [
        "dark",
        "light",
        "high-contrast",
        "retro",
        "nord",
        "mocha",
        "paper",
        "nightfall",
    ] {
        let theme = library
            .get(id)
            .unwrap_or_else(|| panic!("{id} is built in"));
        for name in theme::TOKEN_NAMES {
            let color = theme
                .tokens
                .get(name)
                .unwrap_or_else(|| panic!("{id} has no {name}"));
            assert!(
                color.a > 0.,
                "{id}: {name} is not set, a theme file misses it"
            );
        }
    }
}

/// The theme of one look is dark or light as it says, and the settings offer both.
#[test]
fn the_built_in_themes_cover_both_looks() {
    let library = ThemeLibrary::builtin();
    assert!(library.get("dark").is_some_and(|theme| theme.dark));
    assert!(library.get("light").is_some_and(|theme| !theme.dark));
    assert!(library.get("paper").is_some_and(|theme| !theme.dark));
    for id in ["high-contrast", "retro", "nord", "mocha", "nightfall"] {
        assert!(library.get(id).is_some_and(|theme| theme.dark), "{id}");
    }
    assert_eq!(library.fallback(true), "dark");
    assert_eq!(library.fallback(false), "light");
}

#[test]
fn every_built_in_theme_keeps_the_contrast_of_its_text() {
    let library = ThemeLibrary::builtin();
    for theme in library.themes() {
        for (name, color, background, required) in checks(&theme.tokens) {
            let required = if theme.id == "high-contrast" {
                7.
            } else {
                required
            };
            let ratio = contrast(color, background);
            assert!(
                ratio >= required,
                "{}: {name} {ratio:.2} < {required}",
                theme.id
            );
        }
    }
}

#[test]
fn a_theme_of_the_folder_joins_the_library_over_a_parent() {
    let folder = test_support::TempDir::new("theme-library");
    std::fs::write(
        folder.join("mine.theme.json"),
        r##"{
            "name": "Mine",
            "dark": true,
            "parent": "dark",
            "ui": {"editor_bg": "#101010"},
            "syntax": {"keyword": "#ff8800"}
        }"##,
    )
    .unwrap();
    let library = ThemeLibrary::load(folder.path());
    assert!(library.errors().is_empty(), "{:?}", library.errors());
    let mine = library.get("mine").expect("the file joins the library");
    assert_eq!(mine.name, "Mine");
    assert_eq!(mine.tokens.editor_bg, Color::from_text("#101010").unwrap());
    assert_eq!(mine.tokens.keyword, Color::from_text("#ff8800").unwrap());
    assert_eq!(
        mine.tokens.text,
        library.get("dark").unwrap().tokens.text,
        "the rest comes from the parent"
    );
    assert_eq!(themes_folder(folder.path()), folder.path().join("themes"));
}

#[test]
fn a_broken_file_is_left_out_and_named() {
    let folder = test_support::TempDir::new("theme-broken");
    std::fs::write(folder.join("broken.theme.json"), "{").unwrap();
    std::fs::write(
        folder.join("bad-color.theme.json"),
        r##"{"name": "Bad", "dark": true, "parent": "dark", "ui": {"text": "#zz"}}"##,
    )
    .unwrap();
    std::fs::write(
        folder.join("orphan.theme.json"),
        r##"{"name": "Orphan", "dark": true, "parent": "missing", "ui": {}}"##,
    )
    .unwrap();
    std::fs::write(
        folder.join("dark.theme.json"),
        r##"{"name": "Copy", "dark": true, "parent": "dark", "ui": {}}"##,
    )
    .unwrap();
    let library = ThemeLibrary::load(folder.path());
    assert!(library.get("broken").is_none());
    assert!(library.get("bad-color").is_none());
    assert!(library.get("orphan").is_none());
    assert_eq!(library.get("dark").unwrap().name, "Dark", "a built-in wins");
    let errors: Vec<String> = library.errors().iter().map(ToString::to_string).collect();
    for name in ["broken", "bad-color", "orphan", "dark"] {
        assert!(
            errors.iter().any(|error| error.contains(name)),
            "{name} is named: {errors:?}"
        );
    }
}

#[test]
fn a_short_file_is_valid_for_the_resolver() {
    let file = ThemeFile::parse(
        r##"{"name": "Short", "dark": false, "parent": "light", "ui": {"accent": "#c2410c"}}"##,
    )
    .unwrap();
    let (tokens, warnings) = file.resolve(Some(Tokens::light())).unwrap();
    assert!(warnings.is_empty());
    assert!(!tokens.dark);
    assert_eq!(tokens.accent, Color::from_text("#c2410c").unwrap());
}

#[test]
fn an_imported_name_becomes_a_file_name() {
    assert_eq!(slug("Sample Scheme"), "sample-scheme");
    assert_eq!(slug("  Darcula 2 "), "darcula-2");
    assert_eq!(slug("---"), "imported");
}
