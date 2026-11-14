//! The Profiler tool window without a window and a real recording: a snapshot known in
//! advance paints the flame graph, the call tree and the Method List, a click zooms, a
//! method of the project opens its source, and Profile adds a recording to a plan.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lang_api::{ProfileFrame, ProfileNode, ProfileRecording, ProfileSnapshot};
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{ProfilePlan, RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{PROFILER_TOOL, Workspace};

use crate::profiler::{ActiveProfiler, ProfileColumn, ProfilerTab};
use crate::window_tests::{
    active_path, bounds_of, click, draw, open_sample_window, open_window_with, sample_project,
    scratch,
};
use crate::window_tests_run::{run_panel, setup, wait_for};

const SOURCE: &str = "class Main {\n\tstatic int work(int n) { return n; }\n\tstatic int unused() { return 0; }\n\tpublic static void main(String[] args) { work(1); }\n}\n";

fn frame(class: &str, method: &str, source: Option<&Path>, line: u32) -> ProfileFrame {
    ProfileFrame {
        class: class.to_owned(),
        package: class
            .rsplit_once('.')
            .map_or("", |(package, _)| package)
            .to_owned(),
        method: method.to_owned(),
        signature: "()V".to_owned(),
        source: source.map(Path::to_path_buf),
        line: source.map(|_| line),
    }
}

fn node(frame: ProfileFrame, own: u64, total: u64) -> ProfileNode {
    ProfileNode {
        frame,
        own,
        total,
        children: Vec::new(),
    }
}

/// `main` runs `work` twice and `unused` once: a tree of three samples.
fn snapshot(source: &Path) -> ProfileSnapshot {
    let mut main = node(frame("demo.Main", "main", Some(source), 5), 0, 3);
    main.children = vec![
        node(frame("demo.Main", "work", Some(source), 2), 2, 2),
        node(frame("demo.Main", "unused", Some(source), 3), 1, 1),
    ];
    ProfileSnapshot {
        roots: vec![main],
        samples: 3,
        truncated: false,
    }
}

fn open_profiler(
    cx: &mut TestAppContext,
) -> (
    PathBuf,
    PathBuf,
    WindowHandle<Workspace>,
    gpui::Entity<crate::profiler::ProfilerPanel>,
) {
    let (root, window) = open_sample_window(cx, "profiler");
    let source = root.join("src/Main.code");
    fs::write(&source, SOURCE).unwrap();
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROFILER_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROFILER_TOOL))
            .unwrap(),
        "the Profiler window is open"
    );
    let panel = cx.read(|cx| cx.global::<ActiveProfiler>().0.clone());
    (root, source, window, panel)
}

fn show(
    panel: &gpui::Entity<crate::profiler::ProfilerPanel>,
    snapshot: ProfileSnapshot,
    cx: &mut TestAppContext,
) {
    panel.update(cx, |panel, cx| panel.show_snapshot(snapshot, cx));
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn the_flame_graph_draws_bars_by_share_and_a_click_zooms_a_subtree(cx: &mut TestAppContext) {
    let (_root, source, window, panel) = open_profiler(cx);
    show(&panel, snapshot(&source), cx);
    draw(cx, window);

    let bars = panel.read_with(cx, |panel, _| panel.flame_bars());
    assert_eq!(
        bars.iter()
            .map(|bar| bar.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Main.work", "demo.Main.unused"]
    );
    assert!((bars[0].share - 1.).abs() < f32::EPSILON);
    assert!((bars[1].share - 2. / 3.).abs() < 0.001);
    assert!((bars[2].share - 1. / 3.).abs() < 0.001);
    assert_eq!(bars[1].percent, 66);
    assert!(bars[1].own, "a frame of the project is drawn brighter");

    let root_width = f32::from(bounds_of(cx, window, "flame-0").size.width);
    let work_width = f32::from(bounds_of(cx, window, "flame-0-0").size.width);
    let unused_width = f32::from(bounds_of(cx, window, "flame-0-1").size.width);
    assert!(
        (work_width / root_width - 2. / 3.).abs() < 0.05,
        "the bar of work takes two thirds of the parent: {work_width} of {root_width}"
    );
    assert!(
        (unused_width / root_width - 1. / 3.).abs() < 0.05,
        "the bar of unused takes one third of the parent: {unused_width} of {root_width}"
    );

    window
        .update(cx, |_, window, cx| {
            let focus = panel.read(cx).focus();
            window.focus(&focus, cx);
        })
        .unwrap();
    panel.update(cx, |panel, cx| panel.zoom_to(vec![0, 0], cx));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.zoom().to_vec()),
        vec![0, 0]
    );
    panel.update(cx, |panel, cx| panel.back(cx));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.zoom().to_vec()),
        vec![0]
    );
    panel.update(cx, |panel, cx| panel.back(cx));
    assert!(
        panel.read_with(cx, |panel, _| panel.zoom().is_empty()),
        "Back returns to the whole recording"
    );
}

#[ui_kit::gpui::test]
fn the_call_tree_opens_by_a_click_and_keeps_the_matching_frames_of_a_search(
    cx: &mut TestAppContext,
) {
    let (_root, source, window, panel) = open_profiler(cx);
    show(&panel, snapshot(&source), cx);
    panel.update(cx, |panel, cx| panel.set_tab(ProfilerTab::CallTree, cx));
    draw(cx, window);

    let rows = panel.read_with(cx, |panel, _| panel.call_rows());
    assert_eq!(rows.len(), 1, "only the root is open at first");
    assert!(rows[0].has_children);
    assert!(!rows[0].expanded);
    assert_eq!(rows[0].total, 100);

    panel.update(cx, |panel, cx| panel.toggle_call(&[0], cx));
    let rows = panel.read_with(cx, |panel, _| panel.call_rows());
    assert_eq!(
        rows.iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Main.work", "demo.Main.unused"]
    );
    assert_eq!(rows[1].depth, 1);
    assert_eq!((rows[1].own, rows[1].total), (66, 66));
    assert_eq!((rows[2].own, rows[2].total), (33, 33));

    panel.update(cx, |panel, cx| panel.set_search("unused", cx));
    let rows = panel.read_with(cx, |panel, _| panel.call_rows());
    assert_eq!(
        rows.iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Main.unused"],
        "the search keeps the matches and the frames above them"
    );
    panel.update(cx, |panel, cx| panel.set_search("", cx));
}

#[ui_kit::gpui::test]
fn the_method_list_sorts_by_own_time_and_opens_the_source_of_a_method(cx: &mut TestAppContext) {
    let (_root, source, window, panel) = open_profiler(cx);
    show(&panel, snapshot(&source), cx);
    panel.update(cx, |panel, cx| panel.set_tab(ProfilerTab::MethodList, cx));
    draw(cx, window);

    let rows = panel.read_with(cx, |panel, _| panel.method_rows());
    assert_eq!(
        rows.iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Main.work", "demo.Main.unused"],
        "the total time is the default order"
    );
    assert_eq!((rows[0].own, rows[0].total), (0, 100));

    panel.update(cx, |panel, cx| panel.sort_methods(ProfileColumn::Own, cx));
    let rows = panel.read_with(cx, |panel, _| panel.method_rows());
    assert_eq!(
        rows.iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.work", "demo.Main.unused", "demo.Main.main"],
        "the own time sorts the heaviest first"
    );
    assert_eq!((rows[0].own, rows[0].total), (66, 66));

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.open_method(0, window, cx))
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(active_path(cx, window), Some(source));
}

/// A provider whose profile plan is known in advance and records how it was asked.
struct FixtureProfiler {
    recording: PathBuf,
    snapshot: ProfileSnapshot,
    arguments: Arc<Mutex<Vec<Vec<String>>>>,
}

impl RunConfigProvider for FixtureProfiler {
    fn id(&self) -> &'static str {
        "profiler-fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Profiled".into(),
            provider: self.id().into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(self.plain())
    }

    fn supports_profiler(&self) -> bool {
        true
    }

    fn profile_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<ProfilePlan> {
        let flag = format!(
            "-XX:StartFlightRecording=filename=\"{}\",settings=profile,dumponexit=true",
            self.recording.display()
        );
        let program = ProcessSpec::new("cmd")
            .args(["/d", "/c", "echo", "profiled"])
            .arg(&flag);
        self.arguments
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(
                program
                    .arguments()
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect(),
            );
        Ok(ProfilePlan {
            plan: RunPlan {
                before: None,
                program,
                grace: Duration::from_secs(1),
            },
            recording: self.recording.clone(),
            note: None,
        })
    }

    fn profile_snapshot(
        &self,
        _recording: &Path,
        _model: &ProjectModel,
    ) -> base::Result<ProfileSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn profile_recordings(&self, _model: &ProjectModel) -> base::Result<Vec<ProfileRecording>> {
        Ok(vec![ProfileRecording::named(self.recording.clone())])
    }
}

impl FixtureProfiler {
    fn plain(&self) -> RunPlan {
        RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        }
    }
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn profile_from_the_toolbar_starts_a_recording_and_shows_the_snapshot(cx: &mut TestAppContext) {
    let root = sample_project("profiler-plan");
    let source = root.join("src/Main.code");
    fs::write(&source, SOURCE).unwrap();
    let recording = scratch("profiler-plan-recording").join("App-1.rec");
    let arguments = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureProfiler {
        recording: recording.clone(),
        snapshot: snapshot(&source),
        arguments: arguments.clone(),
    })];
    setup.cache_root = scratch("profiler-plan-cache");
    let window = open_window_with(cx, root, setup);
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_for(cx, "the run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    click(cx, window, "t-profile");
    wait_for(cx, "the profile plan", |_| {
        !arguments
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    });
    wait_for(cx, "the recording of the plan", |cx| {
        panel.read_with(cx, |panel, _| panel.profile_recording().is_some())
    });
    let arguments = arguments
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    assert!(
        arguments[0]
            .iter()
            .any(|argument| argument.starts_with("-XX:StartFlightRecording=")),
        "the plan starts the runtime with a recording: {:?}",
        arguments[0]
    );
    assert!(
        arguments[0]
            .iter()
            .any(|argument| argument.contains("dumponexit=true")),
        "{:?}",
        arguments[0]
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel
            .profile_recording()
            .map(Path::to_path_buf)),
        Some(recording)
    );

    wait_for(cx, "the snapshot of the run", |cx| {
        panel.read_with(cx, |panel, _| panel.profile().is_some())
    });
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROFILER_TOOL))
            .unwrap(),
        "the Profiler window opened with the action"
    );
    draw(cx, window);
    let profiler = cx.read(|cx| cx.global::<ActiveProfiler>().0.clone());
    assert_eq!(
        profiler.read_with(cx, |panel, _| panel
            .flame_bars()
            .iter()
            .map(|bar| bar.label.clone())
            .collect::<Vec<_>>()),
        vec![
            "demo.Main.main".to_owned(),
            "demo.Main.work".to_owned(),
            "demo.Main.unused".to_owned()
        ]
    );
}
