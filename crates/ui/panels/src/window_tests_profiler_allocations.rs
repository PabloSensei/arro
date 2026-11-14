//! The Profiler tool window without a window and a real recording: the switch between the
//! samples of the CPU and the bytes a run allocated, the Allocated Types tab that keeps
//! the stacks of one class, the timeline that keeps an interval of one thread, and the
//! list of several recordings of one run.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::Arc;

use lang_api::{
    ProfileFrame, ProfileNode, ProfileReport, ProfileSample, ProfileSamples, ProfileSnapshot,
    ProfileType, ProfileTypeTree, ProfileUnit,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{PROFILER_TOOL, Workspace};

use crate::profiler::{ActiveProfiler, ProfilerTab, timeline_buckets};
use crate::run::ProfileEntry;
use crate::window_tests::{bounds_of, draw, open_sample_window};

fn frame(class: &str, method: &str) -> ProfileFrame {
    ProfileFrame {
        class: class.to_owned(),
        package: class
            .rsplit_once('.')
            .map_or("", |(package, _)| package)
            .to_owned(),
        method: method.to_owned(),
        signature: "()V".to_owned(),
        source: None,
        line: None,
    }
}

fn node(frame: ProfileFrame, own: u64, total: u64, children: Vec<ProfileNode>) -> ProfileNode {
    ProfileNode {
        frame,
        own,
        total,
        children,
    }
}

fn tree(roots: Vec<ProfileNode>) -> ProfileSnapshot {
    let samples = roots.iter().map(|root| root.total).sum();
    ProfileSnapshot {
        roots,
        samples,
        truncated: false,
    }
}

/// The recording of a program that allocates `byte[]` in `fill` and boxes numbers in
/// `make`, with no CPU samples of its own.
fn allocation_report() -> ProfileReport {
    let byte_array = ProfileTypeTree {
        allocated: ProfileType {
            class: "[B".into(),
            package: String::new(),
            source: None,
            weight: 1_258_291,
            count: 2,
        },
        tree: tree(vec![node(
            frame("demo.Main", "main"),
            0,
            1_258_291,
            vec![node(
                frame("demo.Buffer", "fill"),
                1_258_291,
                1_258_291,
                vec![],
            )],
        )]),
    };
    let boxed = ProfileTypeTree {
        allocated: ProfileType {
            class: "demo.Box".into(),
            package: "demo".into(),
            source: None,
            weight: 4096,
            count: 1,
        },
        tree: tree(vec![node(
            frame("demo.Main", "main"),
            0,
            4096,
            vec![node(frame("demo.Buffer", "make"), 4096, 4096, vec![])],
        )]),
    };
    ProfileReport {
        cpu: ProfileSamples::default(),
        cpu_tree: ProfileSnapshot::default(),
        allocations: Some(tree(vec![node(
            frame("demo.Main", "main"),
            0,
            1_262_387,
            vec![
                node(frame("demo.Buffer", "fill"), 1_258_291, 1_258_291, vec![]),
                node(frame("demo.Buffer", "make"), 4096, 4096, vec![]),
            ],
        )])),
        types: vec![byte_array, boxed],
    }
}

/// Three CPU samples: two of `main`, one of `worker`.
fn cpu_report() -> ProfileReport {
    let samples = ProfileSamples {
        frames: vec![
            frame("demo.Main", "main"),
            frame("demo.Main", "work"),
            frame("demo.Main", "unused"),
            frame("demo.Main", "other"),
        ],
        lanes: vec!["main".into(), "worker".into()],
        samples: vec![
            ProfileSample {
                at: 0,
                lane: 0,
                weight: 1,
                stack: vec![0, 1],
            },
            ProfileSample {
                at: 200,
                lane: 0,
                weight: 1,
                stack: vec![0, 2],
            },
            ProfileSample {
                at: 100,
                lane: 1,
                weight: 1,
                stack: vec![3],
            },
        ],
        unit: ProfileUnit::Samples,
    };
    ProfileReport {
        cpu_tree: samples.snapshot(),
        cpu: samples,
        ..ProfileReport::default()
    }
}

fn entry(label: &str, report: ProfileReport) -> ProfileEntry {
    ProfileEntry {
        label: label.to_owned(),
        report: Arc::new(report),
    }
}

fn open_profiler(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<crate::profiler::ProfilerPanel>,
) {
    let (_root, window) = open_sample_window(cx, "profiler-allocations");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROFILER_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let panel = cx.read(|cx| cx.global::<ActiveProfiler>().0.clone());
    (window, panel)
}

#[ui_kit::gpui::test]
fn the_switch_shows_the_bytes_of_the_allocations_and_their_types(cx: &mut TestAppContext) {
    let (window, panel) = open_profiler(cx);
    panel.update(cx, |panel, cx| {
        panel.show_reports(vec![entry("App-1.jfr", allocation_report())], cx)
    });
    draw(cx, window);

    assert_eq!(
        panel.read_with(cx, |panel, _| panel.mode()),
        crate::profiler::ProfileMode::Cpu
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.flame_bars())
            .is_empty(),
        "the recording has no CPU samples"
    );

    panel.update(cx, |panel, cx| {
        panel.set_mode(crate::profiler::ProfileMode::Allocations, cx)
    });
    draw(cx, window);
    let bars = panel.read_with(cx, |panel, _| panel.flame_bars());
    assert_eq!(
        bars.iter()
            .map(|bar| bar.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Buffer.fill", "demo.Buffer.make"]
    );
    assert_eq!(bars[1].samples, 1_258_291, "the weight is the bytes");
    assert_eq!(bars[0].percent, 100);

    panel.update(cx, |panel, cx| {
        panel.set_tab(ProfilerTab::AllocatedTypes, cx)
    });
    draw(cx, window);
    let rows = panel.read_with(cx, |panel, _| panel.allocation_rows());
    assert_eq!(
        rows.iter()
            .map(|row| row.class.as_str())
            .collect::<Vec<_>>(),
        vec!["[B", "demo.Box"],
        "the heaviest class comes first"
    );
    assert_eq!(rows[0].weight_text, "1.2 MB");
    assert_eq!((rows[0].count, rows[1].count), (2, 1));

    bounds_of(cx, window, "allocated-type-0");
}

#[ui_kit::gpui::test]
fn a_click_on_an_allocated_type_keeps_the_stacks_that_made_it(cx: &mut TestAppContext) {
    let (window, panel) = open_profiler(cx);
    panel.update(cx, |panel, cx| {
        panel.show_reports(vec![entry("App-1.jfr", allocation_report())], cx);
        panel.set_mode(crate::profiler::ProfileMode::Allocations, cx);
        panel.set_tab(ProfilerTab::AllocatedTypes, cx);
    });
    draw(cx, window);

    panel.update(cx, |panel, cx| panel.choose_type(Some("[B"), cx));
    let bars = panel.read_with(cx, |panel, _| panel.flame_bars());
    assert_eq!(
        bars.iter()
            .map(|bar| bar.label.as_str())
            .collect::<Vec<_>>(),
        vec!["demo.Main.main", "demo.Buffer.fill"],
        "the trees keep the stacks of the byte arrays"
    );
    assert_eq!(bars[0].samples, 1_258_291);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel
            .allocation_rows()
            .iter()
            .filter(|row| row.selected)
            .map(|row| row.class.clone())
            .collect::<Vec<_>>()),
        vec!["[B".to_owned()]
    );

    panel.update(cx, |panel, cx| panel.choose_type(None, cx));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.flame_bars().len()),
        3,
        "no class keeps every stack again"
    );
}

#[ui_kit::gpui::test]
fn a_drag_in_the_timeline_keeps_the_samples_of_one_thread_and_interval(cx: &mut TestAppContext) {
    let (window, panel) = open_profiler(cx);
    panel.update(cx, |panel, cx| {
        panel.show_reports(vec![entry("App-1.jfr", cpu_report())], cx)
    });
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.timeline_lanes()),
        vec!["main".to_owned(), "worker".to_owned()]
    );

    panel.update(cx, |panel, cx| {
        panel.set_tab(ProfilerTab::Timeline, cx);
        panel.select_timeline(0, 100, 300, cx);
    });
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.timeline_range()),
        Some((100, 300))
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.timeline_lane()),
        Some(0)
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel
            .flame_bars()
            .iter()
            .map(|bar| bar.label.clone())
            .collect::<Vec<_>>()),
        vec!["demo.Main.main".to_owned(), "demo.Main.unused".to_owned()],
        "the sample at 0 ms is out and the sample of the worker is another thread"
    );

    panel.update(cx, |panel, cx| {
        panel.set_tab(ProfilerTab::FlameGraph, cx);
        panel.reset_timeline(cx);
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.flame_bars().len()),
        4,
        // Two roots, `main` with its two children, and `other`.
        "the button shows every sample again"
    );
    assert!(panel.read_with(cx, |panel, _| panel.timeline_range().is_none()));
}

#[ui_kit::gpui::test]
fn a_run_that_wrote_several_recordings_offers_the_list_above_the_tabs(cx: &mut TestAppContext) {
    let (window, panel) = open_profiler(cx);
    panel.update(cx, |panel, cx| {
        panel.show_reports(
            vec![
                entry("Tests-1.jfr", cpu_report()),
                entry("Tests-2.jfr", allocation_report()),
            ],
            cx,
        )
    });
    draw(cx, window);

    assert_eq!(panel.read_with(cx, |panel, _| panel.report_count()), 2);
    bounds_of(cx, window, "profiler-recording-0");
    bounds_of(cx, window, "profiler-recording-1");

    panel.update(cx, |panel, cx| panel.choose_report(1, cx));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.flame_bars().len()),
        0,
        "the second recording has no CPU samples"
    );
    panel.update(cx, |panel, cx| {
        panel.set_mode(crate::profiler::ProfileMode::Allocations, cx)
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel
            .flame_bars()
            .first()
            .map(|bar| bar.samples)),
        Some(1_262_387)
    );
}

#[ui_kit::gpui::test]
fn the_timeline_never_draws_more_marks_than_the_width_of_a_thread(_cx: &mut TestAppContext) {
    let mut samples = ProfileSamples {
        frames: vec![frame("demo.Main", "main")],
        lanes: vec!["main".into(), "worker".into()],
        samples: Vec::new(),
        unit: ProfileUnit::Samples,
    };
    for index in 0..100_000u64 {
        samples.samples.push(ProfileSample {
            at: index % 10_000,
            lane: (index % 2) as u32,
            weight: 1,
            stack: vec![0],
        });
    }

    let main = timeline_buckets(&samples, 0, 800);

    assert!(main.len() <= 800, "{} marks for 800 pixels", main.len());
    assert_eq!(main.iter().map(|bucket| bucket.count).sum::<u64>(), 50_000);
    assert_eq!(timeline_buckets(&samples, 1, 1).len().min(2), 1);
    let all: f32 = main.iter().map(|bucket| bucket.share).sum();
    assert!((all - 1.).abs() < 0.001, "{all}");
}
