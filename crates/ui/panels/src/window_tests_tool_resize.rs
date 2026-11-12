//! T-0146: the size of a tool window from the keyboard, Fully Expand in the project
//! tree, Synchronize after an outside change, and a build file of a new module.
//!
//! cargo test -p panels window_tests_tool_resize

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::Languages;
use lang_api::{
    FileChange, LanguageFn, LanguageId, LanguageSupport, Queries, SemanticServer, SyntaxProvider,
};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{
    DOCUMENTATION_TOOL, DockSide, FileIcons, Launch as WindowLaunch, Layout, MIN_DOCK_SIZE,
    Settings, Workspace,
};

use crate::index::SearchSetup;
use crate::project::{ActiveProjectPanel, FULL_EXPAND_MAX_DEPTH};
use crate::run::ProjectSetup;
use crate::window_tests::{
    buffer_of, draw, open_path, open_sample_window, open_window, press, project_setup, scratch,
};
use crate::window_tests_menus::show_project;
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language that watches `pom.xml` and keeps what it was told about them.
struct BuildFileLanguage {
    syntax: NoSyntax,
    watched: Mutex<Vec<(String, FileChange)>>,
}

impl BuildFileLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            watched: Mutex::new(Vec::new()),
        })
    }
}

impl SemanticServer for BuildFileLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}
    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<lang_api::DocumentEdit>) {}
    fn save(&self, _: &str) {}
    fn close(&self, _: &str) {}

    fn file_changed(&self, uri: &str, change: FileChange) {
        self.watched.lock().unwrap().push((uri.to_owned(), change));
    }

    fn version(&self, _: &str) -> Option<i32> {
        None
    }
}

impl LanguageSupport for BuildFileLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("build-file")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn watches_file(&self, path: &Path) -> bool {
        path.file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("pom.xml"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }
}

/// A build tool that counts the imports of the project model.
struct BuildProbe {
    model: ProjectModel,
    syncs: Arc<AtomicUsize>,
}

impl BuildSystem for BuildProbe {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// The window of the fake language and the build tool of the caller.
fn open_build_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<BuildFileLanguage>,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("tool-resize-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(ui_kit::Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = WindowLaunch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
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

fn dock_size(cx: &TestAppContext, window: WindowHandle<Workspace>, side: DockSide) -> f32 {
    window
        .read_with(cx, |workspace, _| workspace.dock_size(side))
        .unwrap()
}

fn layout_dock_size(cx: &TestAppContext, window: WindowHandle<Workspace>, side: DockSide) -> f32 {
    window
        .read_with(cx, |workspace, cx| {
            let layout = workspace.current_layout(cx);
            match side {
                DockSide::Left => layout.left.size,
                DockSide::Right => layout.right.size,
                DockSide::Bottom => layout.bottom.size,
            }
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn ctrl_shift_left_and_right_resize_the_tool_windows(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "tool-resize-keys");
    show_project(cx, window);

    // The left window grows to the right and stops at the smallest size.
    let before = dock_size(cx, window, DockSide::Left);
    press(cx, window, "ctrl-alt-shift-right");
    let wider = dock_size(cx, window, DockSide::Left);
    assert!(
        wider > before,
        "Ctrl+Alt+Shift+Right widens the left window"
    );
    assert_eq!(layout_dock_size(cx, window, DockSide::Left), wider);

    for _ in 0..40 {
        press(cx, window, "ctrl-shift-left");
    }
    let smallest = dock_size(cx, window, DockSide::Left);
    assert!(
        (smallest - MIN_DOCK_SIZE).abs() < 0.01,
        "the left window stops at {MIN_DOCK_SIZE}: {smallest}"
    );

    // The right window grows when its inner edge moves left.
    show_tool(cx, window, DOCUMENTATION_TOOL);
    let before = dock_size(cx, window, DockSide::Right);
    press(cx, window, "ctrl-shift-left");
    let wider = dock_size(cx, window, DockSide::Right);
    assert!(wider > before, "Ctrl+Shift+Left widens the right window");
    assert_eq!(layout_dock_size(cx, window, DockSide::Right), wider);

    for _ in 0..40 {
        press(cx, window, "ctrl-alt-shift-right");
    }
    let smallest = dock_size(cx, window, DockSide::Right);
    assert!(
        (smallest - MIN_DOCK_SIZE).abs() < 0.01,
        "the right window stops at {MIN_DOCK_SIZE}: {smallest}"
    );
}

#[ui_kit::gpui::test]
fn fully_expand_opens_the_project_tree_down_to_the_depth_limit(cx: &mut TestAppContext) {
    let root = scratch("tool-resize-expand");
    let mut folder = root.clone();
    for level in 0..FULL_EXPAND_MAX_DEPTH + 4 {
        folder = folder.join(format!("level{level}"));
    }
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("deep.txt"), "deep\n").unwrap();
    fs::write(root.join("main.txt"), "main\n").unwrap();

    let window = open_window(cx, root);
    show_project(cx, window);
    press(cx, window, "numpad_multiply");

    let panel = cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    wait_for(cx, "the whole project tree", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .tree()
                .rows()
                .iter()
                .map(|row| row.depth)
                .max()
                .is_some_and(|depth| depth >= FULL_EXPAND_MAX_DEPTH)
        })
    });

    let rows = panel.read_with(cx, |panel, _| panel.tree().rows().to_vec());
    let deepest = rows.iter().map(|row| row.depth).max().unwrap_or(0);
    assert_eq!(
        deepest, FULL_EXPAND_MAX_DEPTH,
        "a folder deeper than {FULL_EXPAND_MAX_DEPTH} stays closed"
    );
    assert!(
        rows.iter()
            .any(|row| row.folder && row.open && row.depth < FULL_EXPAND_MAX_DEPTH),
        "the folders above the limit are open"
    );
}

#[ui_kit::gpui::test]
fn synchronize_reads_a_file_that_changed_outside(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "tool-resize-sync");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = crate::window_tests::editor_of(cx, window, &path);
    fs::write(&path, "ALPHA\nbeta\n").unwrap();

    press(cx, window, "ctrl-alt-y");

    wait_for(cx, "the text from disk", |cx| {
        buffer_of(cx, &view) == "ALPHA\nbeta\n"
    });
}

#[ui_kit::gpui::test]
fn a_new_build_file_in_a_subfolder_imports_the_project_again(cx: &mut TestAppContext) {
    let root = scratch("tool-resize-import");
    let module = root.join("module");
    fs::create_dir_all(&module).unwrap();
    fs::write(root.join("Main.txt"), "main\n").unwrap();

    let syncs = Arc::new(AtomicUsize::new(0));
    let model = ProjectModel {
        root: root.clone(),
        tool: "fixture".into(),
        resolved: true,
        ..ProjectModel::default()
    };
    let mut setup = project_setup(scratch("tool-resize-import-cache"));
    setup.tool = Some(Arc::new(BuildProbe {
        model,
        syncs: syncs.clone(),
    }));
    let language = BuildFileLanguage::new();
    let _window = open_build_window(cx, root.clone(), language.clone(), setup);
    cx.run_until_parked();
    // The watcher starts before the first scan; an event made right after the window
    // opens still arrives.
    std::thread::sleep(Duration::from_millis(300));
    wait_for(cx, "the first import", |_| {
        syncs.load(Ordering::SeqCst) >= 1
    });
    let before = syncs.load(Ordering::SeqCst);

    fs::write(module.join("pom.xml"), "<project/>\n").unwrap();

    wait_for(cx, "the import of the new module", |_| {
        syncs.load(Ordering::SeqCst) > before
    });
    assert!(
        language
            .watched
            .lock()
            .unwrap()
            .iter()
            .any(|(uri, change)| *change == FileChange::Created && uri.ends_with("pom.xml")),
        "the server heard about the created build file"
    );
}
