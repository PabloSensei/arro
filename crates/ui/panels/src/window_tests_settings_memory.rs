//! The search of the Settings window opens the page the query names, and the Home list of
//! the page of the first build tool offers the same three sources as the second one.

use project_model::ToolHome;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::settings_catalog::{self, SettingsPage, set_build_tool_names};
use crate::window_tests::{click, draw, open_sample_window, press, type_text};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The selector of the row of `page` in the tree, on the row that is the open page.
fn chosen(page: SettingsPage) -> &'static str {
    Box::leak(
        format!(
            "settings-page-chosen-{}",
            page.title().to_lowercase().replace(' ', "-")
        )
        .into_boxed_str(),
    )
}

fn option(id: &str) -> &'static str {
    Box::leak(id.to_owned().into_boxed_str())
}

/// The names the language gives its two build tools, before any window asks for them.
fn install_names() {
    let (first, second) = window_language::build_tool_names();
    set_build_tool_names(first, second);
}

/// Opens Settings with Ctrl+Alt+S, as the user does: the window opens on Appearance with
/// the caret in the search field.
fn open_settings(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    install_names();
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    assert!(has(cx, window, "settings"), "the Settings window is drawn");
    window
}

/// Opens Settings on the page of the first build tool, as the missing tool notification
/// does, and draws it.
fn open_first_tool_page(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    install_names();
    let (_root, window) = open_sample_window(cx, name);
    window
        .update(cx, |workspace, window, cx| {
            crate::settings::SettingsView::show_at(
                workspace,
                window,
                cx,
                Some("build_tool_first_home"),
            );
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(has(cx, window, "settings"), "the Settings window is drawn");
    assert!(
        has(cx, window, "settings-build-tool-first-home"),
        "the Home row is on the page of the first build tool"
    );
    window
}

#[gpui::test]
fn the_search_of_memory_opens_the_memory_page(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-memory-search");

    type_text(cx, window, "memory");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::Memory)),
        "the page named by the query opens"
    );
    assert!(
        has(cx, window, "settings-memory-low-check"),
        "the Memory page shows its settings"
    );
}

#[gpui::test]
fn the_search_of_a_build_tool_name_opens_its_page(cx: &mut TestAppContext) {
    let (first, second) = window_language::build_tool_names();
    let window = open_settings(cx, "settings-second-tool-search");

    type_text(cx, window, &second);
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::BuildToolsSecond)),
        "the page named by the query opens"
    );

    let window = open_settings(cx, "settings-first-tool-search");
    type_text(cx, window, &first);
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::BuildToolsFirst)),
        "the page named by the query opens"
    );
}

#[gpui::test]
fn a_query_of_a_field_hint_opens_the_page_of_the_field(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-hint-search");

    type_text(cx, window, "xmx2g");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::BuildToolsFirst)),
        "the page of the field with the hint opens"
    );
    assert!(
        has(cx, window, "settings-build-tool-first-vm-options"),
        "the field of the hint is on that page"
    );
}

#[gpui::test]
fn the_home_list_of_the_first_tool_offers_the_three_sources(cx: &mut TestAppContext) {
    let window = open_first_tool_page(cx, "settings-first-home-list");

    click(cx, window, "settings-build-tool-first-home");
    draw(cx, window);

    assert!(
        has(cx, window, option("settings-choice-option-wrapper")),
        "the wrapper of the project"
    );
    assert!(
        has(
            cx,
            window,
            option("settings-choice-option-local-installation")
        ),
        "a local installation"
    );
    assert!(
        has(
            cx,
            window,
            option("settings-choice-option-downloaded-by-arro")
        ),
        "the copy the IDE downloaded"
    );
    assert!(
        !has(cx, window, "settings-build-tool-first-home-kind"),
        "the Home source row is merged into the list"
    );
}

#[gpui::test]
fn local_installation_shows_the_path_field_of_the_first_tool(cx: &mut TestAppContext) {
    let window = open_first_tool_page(cx, "settings-first-home-path");
    assert!(
        !has(cx, window, "settings-build-tool-first-home-path"),
        "the wrapper is the default source, so the path is hidden"
    );

    click(cx, window, "settings-build-tool-first-home");
    draw(cx, window);
    click(
        cx,
        window,
        option("settings-choice-option-local-installation"),
    );
    draw(cx, window);

    assert!(
        has(cx, window, "settings-build-tool-first-home-path"),
        "a local installation needs its folder"
    );
    assert!(
        has(cx, window, "settings-build-tool-first-home-browse"),
        "the folder comes with a button that asks for it"
    );
}

#[test]
fn the_page_named_by_the_query_comes_before_a_page_of_its_words() {
    install_names();
    let (first, second) = window_language::build_tool_names();

    assert_eq!(
        settings_catalog::page_matching("memory"),
        Some(SettingsPage::Memory),
        "the page whose name is the query wins over a page with the word in its settings"
    );
    assert_eq!(
        settings_catalog::page_matching(&second),
        Some(SettingsPage::BuildToolsSecond)
    );
    assert_eq!(
        settings_catalog::page_matching(&first),
        Some(SettingsPage::BuildToolsFirst)
    );
    assert_eq!(
        settings_catalog::page_matching("mem"),
        Some(SettingsPage::Memory),
        "a start of a page name comes next"
    );
    assert_eq!(
        settings_catalog::page_matching("xmx2g"),
        Some(SettingsPage::BuildToolsFirst),
        "a query of a field hint opens the page of the field"
    );
}

#[test]
fn the_home_list_of_the_first_tool_reads_the_old_folder_as_a_local_installation() {
    let mut settings = Settings::default();
    assert_eq!(
        settings_catalog::choice_label(&settings, "build_tool_first_home").as_deref(),
        Some("Wrapper"),
        "an empty value of an old file is the wrapper"
    );
    assert_eq!(settings_catalog::choices("build_tool_first_home").len(), 3);

    settings.build_tools.first.home = "C:/tool-dist".into();
    assert_eq!(
        settings_catalog::choice_label(&settings, "build_tool_first_home").as_deref(),
        Some("Local installation"),
        "a folder of an old file is a local installation"
    );

    settings.build_tools.first.home.clear();
    settings.build_tools.first.home_kind = ToolHome::Bundled;
    assert_eq!(
        settings_catalog::choice_label(&settings, "build_tool_first_home").as_deref(),
        Some("Downloaded by Arro")
    );

    // A file with only a folder shows Local installation; picking the Wrapper of the list
    // has to store that choice even though the row already showed it.
    settings.build_tools.first.home = "C:/tool-dist".into();
    settings.build_tools.first.home_kind = ToolHome::Auto;
    assert!(settings_catalog::set_choice(
        &mut settings,
        "build_tool_first_home",
        0
    ));
    assert_eq!(settings.build_tools.first.home_kind, ToolHome::Wrapper);
    assert!(
        !settings.build_tools.first.home.is_empty(),
        "the folder stays for the next local installation"
    );
}
