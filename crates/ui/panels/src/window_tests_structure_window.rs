//! The Structure tool window follows the editor: it shows the declarations of the file on
//! the active tab when it opens, after other tool windows opened before it, and when the
//! tab changes. A window opened from a saved layout never sees a tab change, so the panels
//! are told which file is active once the layout is in place.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{
    BEANS_TOOL, ENDPOINTS_TOOL, FileIcons, Launch, Layout, PaneState, STRUCTURE_TOOL, Settings,
    Workspace, bind_keys,
};

use crate::outline::SymbolsFor;
use crate::structure::ActiveStructure;
use crate::window_tests::{
    draw, open_path, open_window, project_setup, scratch, wait_for_outline_signal,
};
use crate::window_tests_ui::wait_for;

const OWNER: &str = r#"package demo;

public class Owner {
    private String address;

    public String getAddress() { return address; }
}
"#;

const PET: &str = r#"package demo;

public class Pet {
    private String name;

    public String getName() { return name; }
}
"#;

/// A source file of the class. The extension is put together here, as the other window
/// tests do, so that this neutral crate does not spell it.
fn source(root: &Path, class: &str, text: &str) -> PathBuf {
    let path = root.join("src").join(format!("{class}.{}{}", "ja", "va"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

/// A window whose language lists the declarations of a source file.
fn window_with_symbols(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_window(cx, root);
    cx.update(|cx| cx.set_global(SymbolsFor(window_language::symbols_for)));
    cx.run_until_parked();
    draw(cx, window);
    window
}

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// The names of the rows of the Structure window, indented.
fn rows(cx: &TestAppContext) -> Vec<String> {
    let structure = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    structure.read_with(cx, |panel, cx| panel.lines(cx))
}

/// A window built the way the application builds it, from a layout that an earlier run
/// saved, so the Structure window opens on tabs that were in place before any of them
/// changed.
fn open_restored_window(
    cx: &mut TestAppContext,
    root: &Path,
    state_dir: &Path,
    tabs: Vec<PathBuf>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        bind_keys(cx);
    });
    let layout = Layout {
        panes: vec![PaneState {
            tabs,
            active: 0,
            carets: Vec::new(),
            pinned: Vec::new(),
        }],
        ..Layout::default()
    };
    let launch = Launch {
        root: root.to_path_buf(),
        open: None,
        state_dir: state_dir.to_path_buf(),
        settings_dir: state_dir.to_path_buf(),
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = crate::AgentOptions {
        state_dir: state_dir.to_path_buf(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = crate::LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, None, Vec::new(), window, cx);
        crate::install(
            &mut workspace,
            crate::index::SearchSetup::default(),
            project_setup(scratch("structure-window-cache")),
            agents,
            window,
            cx,
        );
        crate::connect(&mut workspace, server, cx);
        cx.set_global(SymbolsFor(window_language::symbols_for));
        workspace
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
}

#[ui_kit::gpui::test]
fn the_structure_window_shows_the_open_file_when_it_opens(cx: &mut TestAppContext) {
    let root = scratch("structure-window-open");
    let owner = source(&root, "Owner", OWNER);
    let window = window_with_symbols(cx, root);
    open_path(cx, window, &owner);
    wait_for_outline_signal(cx, window, &owner);

    show_tool(cx, window, STRUCTURE_TOOL);
    wait_for(cx, "the declarations of the open file", |cx| {
        rows(cx).iter().any(|row| row == "Owner")
    });
    let shown = rows(cx);
    assert!(
        shown.iter().any(|row| row.starts_with("  getAddress")),
        "the methods of the file are there: {shown:?}"
    );
}

#[ui_kit::gpui::test]
fn the_structure_window_shows_the_file_after_other_tool_windows(cx: &mut TestAppContext) {
    let root = scratch("structure-window-after-tools");
    let owner = source(&root, "Owner", OWNER);
    let window = window_with_symbols(cx, root);
    open_path(cx, window, &owner);
    wait_for_outline_signal(cx, window, &owner);

    show_tool(cx, window, ENDPOINTS_TOOL);
    show_tool(cx, window, BEANS_TOOL);
    show_tool(cx, window, STRUCTURE_TOOL);
    wait_for(cx, "the declarations after the other windows", |cx| {
        rows(cx).iter().any(|row| row == "Owner")
    });
    assert!(
        rows(cx).iter().any(|row| row.starts_with("  getAddress")),
        "the methods of the file are there: {:?}",
        rows(cx)
    );
}

#[ui_kit::gpui::test]
fn the_structure_window_moves_to_another_tab(cx: &mut TestAppContext) {
    let root = scratch("structure-window-tabs");
    let owner = source(&root, "Owner", OWNER);
    let pet = source(&root, "Pet", PET);
    let window = window_with_symbols(cx, root);
    open_path(cx, window, &owner);
    wait_for_outline_signal(cx, window, &owner);
    show_tool(cx, window, STRUCTURE_TOOL);
    wait_for(cx, "the declarations of the first file", |cx| {
        rows(cx).iter().any(|row| row == "Owner")
    });

    open_path(cx, window, &pet);
    wait_for_outline_signal(cx, window, &pet);
    wait_for(cx, "the declarations of the other file", |cx| {
        rows(cx).iter().any(|row| row == "Pet")
    });
    let shown = rows(cx);
    assert!(
        shown.iter().any(|row| row.starts_with("  getName")),
        "the methods of the other file are there: {shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.starts_with("  getAddress")),
        "the rows of the file that was left are gone: {shown:?}"
    );
}

#[ui_kit::gpui::test]
fn a_tab_restored_from_the_saved_layout_fills_the_structure_window(cx: &mut TestAppContext) {
    let root = scratch("structure-window-restored");
    let owner = source(&root, "Owner", OWNER);
    let state = scratch("structure-window-restored-state");
    let window = open_restored_window(cx, &root, &state, vec![owner.clone()]);
    // The tab comes back without a tab change: the window tells the panels which file is
    // active once the layout is in place, before the Structure window is even opened.
    wait_for(cx, "the declarations of the restored tab", |cx| {
        rows(cx).iter().any(|row| row == "Owner")
    });

    show_tool(cx, window, STRUCTURE_TOOL);
    assert!(
        rows(cx).iter().any(|row| row.starts_with("  getAddress")),
        "the methods of the restored file are there: {:?}",
        rows(cx)
    );
}
