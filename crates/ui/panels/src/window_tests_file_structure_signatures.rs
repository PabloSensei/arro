//! The signatures and the visibility of a source file in the File Structure popup and in the
//! Structure window: the overloads of a method have their parameter types, a field has its
//! type, a mark shows the visibility, and the filter looks at the names alone.

use std::fs;
use std::path::PathBuf;

use editor_view::{OutlineItem, OutlineKind};
use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::px;
use workspace::Workspace;

use crate::outline::SymbolsFor;
use crate::palette::Palette;
use crate::structure::ActiveStructure;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_palette, open_path, open_window, press, scratch, type_text,
};
use crate::window_tests_ui::wait_for;

const OWNER: &str = r#"package demo;

public class Owner {
    private String address;
    private Pet pet;

    public Owner(String name) {}

    public String getAddress() { return address; }
    public Pet getPet(String name) { return pet; }
    public Pet getPet(Integer id) { return pet; }
    public Pet getPet(String name, boolean active) { return pet; }
    protected void keep(String... names) {}
    void noop() {}
}
"#;

/// The file of the class, `Owner` with the extension of the language. The extension is put
/// together here, as the other window tests do, so that this neutral crate does not spell it.
fn owner_file() -> String {
    format!("Owner.{}{}", "ja", "va")
}

/// A window on the file of the class, with the declarations read by the language of the
/// application: the signatures and the visibility come from the tree of the file.
fn owner_window(cx: &mut TestAppContext, name: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = scratch(name);
    let source = root.join("src").join(owner_file());
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, OWNER).unwrap();
    let window = open_window(cx, root);
    cx.update(|cx| cx.set_global(SymbolsFor(window_language::symbols_for)));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &source);
    let structure = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    wait_for(cx, "the outline of the file", |cx| {
        !structure
            .read_with(cx, |panel, cx| panel.lines(cx))
            .is_empty()
    });
    (source, window)
}

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

#[ui_kit::gpui::test]
fn the_file_structure_names_the_signatures_of_the_overloads_and_the_fields(
    cx: &mut TestAppContext,
) {
    let (_, window) = owner_window(cx, "file-structure-signatures");
    open_palette(cx, window, keys::FILE_STRUCTURE);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    for (name, detail) in [
        ("getPet", "(String): Pet"),
        ("getPet", "(Integer): Pet"),
        ("getPet", "(String, boolean): Pet"),
        ("getAddress", "(): String"),
        ("Owner", "(String)"),
        ("address", ": String"),
        ("pet", ": Pet"),
        ("keep", "(String...): void"),
    ] {
        assert!(
            rows.contains(&(name.to_owned(), detail.to_owned())),
            "{name}{detail} is not among the rows {rows:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn the_filter_finds_a_name_and_not_a_type(cx: &mut TestAppContext) {
    let (_, window) = owner_window(cx, "file-structure-filter");
    open_palette(cx, window, keys::FILE_STRUCTURE);
    type_text(cx, window, "getpet");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert_eq!(rows.len(), 3, "the three overloads: {rows:?}");
    assert!(rows.iter().all(|(name, _)| name == "getPet"), "{rows:?}");

    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "String");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.is_empty(),
        "a type in a signature is not a name: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn the_marks_show_the_visibility_of_each_declaration(cx: &mut TestAppContext) {
    let (_, window) = owner_window(cx, "file-structure-marks");
    open_palette(cx, window, keys::FILE_STRUCTURE);
    let marks = palette(cx, window).read_with(cx, |palette, _| palette.declaration_marks());
    for expected in [
        ("address", "general/lock"),
        ("getPet", "nodes/c_public"),
        ("keep", "nodes/c_protected"),
        ("noop", "nodes/c_plocal"),
    ] {
        assert!(
            marks.contains(&(expected.0.to_owned(), expected.1)),
            "{expected:?} is not among the marks {marks:?}"
        );
    }
    assert!(
        bounds_of(cx, window, "palette-badge-0").size.width > px(0.),
        "a mark stands beside the icon of the kind"
    );
}

#[ui_kit::gpui::test]
fn server_names_with_parameters_show_the_name_and_filter_by_it(cx: &mut TestAppContext) {
    let root = scratch("file-structure-server-names");
    let file = root.join("src").join("Owner.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "class Owner\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &file);
    cx.run_until_parked();
    // Some servers name a method with its parameters and give the result as the signature.
    // No provider of the window reads this file, so the outline stays as it is set here.
    let items = [
        ("getPet(String)", "getPet(String): Pet", 0),
        ("getPet(Integer)", "getPet(Integer): Pet", 20),
        (
            "getPet(String, boolean)",
            "getPet(String, boolean): Pet",
            40,
        ),
    ]
    .map(|(name, signature, at)| OutlineItem {
        name: name.to_owned(),
        signature: Some(signature.to_owned()),
        visibility: None,
        kind: OutlineKind::Method,
        depth: 0,
        range: at..at + 10,
        name_range: at..at + 7,
    });
    let view = editor_of(cx, window, &file);
    view.update(cx, |view, cx| view.set_outline(items.to_vec(), cx));
    cx.run_until_parked();
    open_palette(cx, window, keys::FILE_STRUCTURE);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.contains(&("getPet".to_owned(), "(String): Pet".to_owned())),
        "the title is the name and the grey part the rest of the signature: {rows:?}"
    );

    type_text(cx, window, "string");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.is_empty(),
        "the parameter of a name does not match the filter: {rows:?}"
    );

    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace",
    );
    type_text(cx, window, "getpet");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert_eq!(rows.len(), 3, "the three overloads: {rows:?}");
}

#[ui_kit::gpui::test]
fn the_structure_window_lists_the_signatures_with_their_marks(cx: &mut TestAppContext) {
    let (_, _window) = owner_window(cx, "file-structure-window");
    let structure = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    let lines = structure.read_with(cx, |panel, cx| panel.lines(cx));
    for expected in [
        "getPet(String): Pet",
        "getPet(Integer): Pet",
        "getPet(String, boolean): Pet",
        "address: String",
    ] {
        assert!(
            lines.iter().any(|line| line.trim() == expected),
            "{expected} is not among the rows {lines:?}"
        );
    }
    let marks = structure.read_with(cx, |panel, cx| panel.visibility_marks(cx));
    assert_eq!(marks.len(), lines.len(), "one mark per row");
    let at = |label: &str| {
        lines
            .iter()
            .position(|line| line.trim() == label)
            .unwrap_or_else(|| panic!("{label} is among the rows {lines:?}"))
    };
    assert_eq!(marks[at("address: String")], "general/lock");
    assert_eq!(marks[at("getPet(Integer): Pet")], "nodes/c_public");
}
