//! The tab of a heap dump without a real JVM: a small file written by the test is read by
//! the real reader, and the window shows the histogram, the filter, the instances with
//! their fields and the path to the root.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;

use base::Cancel;
use project_model::ProjectModel;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::heap_dump::{HeapColumn, HeapDumpView, HeapSection};
use crate::window_tests::{click, draw, open_window, scratch};

const TAG_STRING: u8 = 0x01;
const TAG_LOAD_CLASS: u8 = 0x02;
const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;
const ROOT_THREAD_OBJECT: u8 = 0x08;
const CLASS_DUMP: u8 = 0x20;
const INSTANCE_DUMP: u8 = 0x21;
const OBJ_ARRAY_DUMP: u8 = 0x22;

/// Writes a heap with `demo.Main` twice, `demo.Other` once and a root array that holds the
/// two instances of `Main`. Every identifier is four bytes.
fn heap_dump() -> Vec<u8> {
    let mut top = Vec::new();
    let mut heap = Vec::new();
    let id = |value: u64| (value as u32).to_be_bytes().to_vec();

    let mut record = |tag: u8, body: &[u8]| {
        top.push(tag);
        top.extend_from_slice(&0u32.to_be_bytes());
        top.extend_from_slice(&(body.len() as u32).to_be_bytes());
        top.extend_from_slice(body);
    };
    let mut low = |tag: u8, body: &[u8]| {
        heap.push(tag);
        heap.extend_from_slice(body);
    };

    for (number, text) in [
        (1, "demo/Main"),
        (2, "value"),
        (3, "demo/Other"),
        (4, "[Ldemo/Main;"),
    ] {
        let mut body = id(number);
        body.extend_from_slice(text.as_bytes());
        record(TAG_STRING, &body);
    }
    for (class, name) in [(0x100, 1), (0x101, 3), (0x102, 4)] {
        let mut body = 0u32.to_be_bytes().to_vec();
        body.extend_from_slice(&id(class));
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&id(name));
        record(TAG_LOAD_CLASS, &body);
    }
    let mut class_dump = |class: u64, size: u32, fields: &[(u64, u8)]| {
        let mut body = id(class);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&id(0));
        for _ in 0..5 {
            body.extend_from_slice(&id(0));
        }
        body.extend_from_slice(&size.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (name, kind) in fields {
            body.extend_from_slice(&id(*name));
            body.push(*kind);
        }
        low(CLASS_DUMP, &body);
    };
    class_dump(0x100, 4, &[(2, 10)]);
    class_dump(0x101, 4, &[]);
    let mut instance = |address: u64, class: u64, bytes: &[u8]| {
        let mut body = id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&id(class));
        body.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        body.extend_from_slice(bytes);
        low(INSTANCE_DUMP, &body);
    };
    instance(0x201, 0x100, &7i32.to_be_bytes());
    instance(0x202, 0x100, &9i32.to_be_bytes());
    instance(0x203, 0x101, &[]);
    let mut array = id(0x300);
    array.extend_from_slice(&0u32.to_be_bytes());
    array.extend_from_slice(&2u32.to_be_bytes());
    array.extend_from_slice(&id(0x102));
    array.extend_from_slice(&id(0x201));
    array.extend_from_slice(&id(0x202));
    low(OBJ_ARRAY_DUMP, &array);
    let mut root = id(0x300);
    root.extend_from_slice(&0u32.to_be_bytes());
    root.extend_from_slice(&0u32.to_be_bytes());
    low(ROOT_THREAD_OBJECT, &root);

    let mut out = Vec::new();
    out.extend_from_slice(&[
        0x4A, 0x41, 0x56, 0x41, 0x20, 0x50, 0x52, 0x4F, 0x46, 0x49, 0x4C, 0x45, 0x20, 0x31, 0x2E,
        0x30, 0x2E, 0x32, 0x00,
    ]);
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&top);
    out.push(TAG_HEAP_DUMP_SEGMENT);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(heap.len() as u32).to_be_bytes());
    out.extend_from_slice(&heap);
    out
}

fn open_heap(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &std::path::Path,
) -> gpui::Entity<HeapDumpView> {
    let snapshot = window_language::read_heap_dump(
        path,
        &ProjectModel::default(),
        &Cancel::new(),
        &mut |_| {},
    )
    .expect("the test file is a heap dump");
    let path = path.to_path_buf();
    window
        .update(cx, |workspace, window, cx| {
            HeapDumpView::open_snapshot(workspace, path, snapshot, window, cx)
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_tab_lists_the_classes_the_instances_their_fields_and_the_path_to_a_root(
    cx: &mut TestAppContext,
) {
    let root = scratch("heap-dump");
    let path = root.join("demo.hprof");
    fs::write(&path, heap_dump()).unwrap();
    let window = open_window(cx, root);
    let view = open_heap(cx, window, &path);
    draw(cx, window);

    let active = window
        .read_with(cx, |workspace, _| workspace.active_view())
        .unwrap()
        .expect("the heap dump is the active tab");
    assert!(
        active.downcast::<HeapDumpView>().is_ok(),
        "the tab shows the heap dump view"
    );

    let rows = view.read_with(cx, |view, _| view.class_rows());
    let main = rows
        .iter()
        .find(|row| row.name == "demo.Main")
        .expect("the class of the project");
    assert_eq!(main.count, 2);
    assert!(main.retained >= main.shallow);
    assert_eq!(main.package, "demo");

    click(cx, window, "heap-sort-retained");
    assert_eq!(
        view.read_with(cx, |view, _| view.sort()),
        (HeapColumn::Retained, false),
        "a click on the chosen column flips its order"
    );
    view.update(cx, |view, cx| view.set_sort(HeapColumn::Name, false, cx));
    let rows = view.read_with(cx, |view, _| view.class_rows());
    assert_eq!(rows[0].name, "demo.Main", "names sort from A to Z");

    view.update(cx, |view, cx| view.set_filter("demo", cx));
    assert_eq!(
        view.read_with(cx, |view, _| view.class_rows()).len(),
        3,
        "the two classes and the array of Main"
    );
    view.update(cx, |view, cx| view.set_filter("demo.Other", cx));
    let rows = view.read_with(cx, |view, _| view.class_rows());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "demo.Other");
    view.update(cx, |view, cx| view.set_filter("", cx));

    let main_index = view.read_with(cx, |view, _| {
        view.class_rows()
            .iter()
            .find(|row| row.name == "demo.Main")
            .map(|row| row.index)
            .unwrap()
    });
    view.update(cx, |view, cx| view.choose_class(Some(main_index), cx));
    let instances = view.read_with(cx, |view, _| view.instance_rows());
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].retained, 4);

    let first = instances[0].object;
    view.update(cx, |view, cx| view.choose_object(Some(first), cx));
    assert!(view.read_with(cx, |view, _| view.expanded(first)));
    let fields = view.read_with(cx, |view, _| view.field_rows());
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "value");
    assert!(
        fields[0].value == "7" || fields[0].value == "9",
        "{}",
        fields[0].value
    );

    let path = view.read_with(cx, |view, _| view.path_rows());
    assert_eq!(path.len(), 2, "the root array and the instance");
    assert!(path[0].root);
    assert!(path[1].target);

    view.update(cx, |view, cx| view.set_section(HeapSection::Dominators, cx));
    let dominators = view.read_with(cx, |view, _| view.dominator_rows());
    assert_eq!(
        dominators[0].retained, 36,
        "the root array dominates both instances"
    );
    assert!(dominators.iter().any(|row| row.depth > 0));
    draw(cx, window);
}
