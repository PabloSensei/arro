//! The unit tests of the place of a frame in the frames list: the percent escapes of a
//! frame path and the `method:line, Class (package)` form of every frame, whether its
//! class comes from the name, from a runtime image, an archive or a `jdt://` buffer.

use std::path::{Path, PathBuf};

use dap_client::Frame;

use super::{decode_uri, frame_place};

fn frame(name: &str, source: Option<&str>, line: u32) -> Frame {
    Frame {
        id: 1,
        name: name.to_owned(),
        source: source.map(PathBuf::from),
        line,
        column: 0,
    }
}

/// The extension of a file of the tests. The parsing does not depend on it, so the tests
/// use a neutral one.
fn extension() -> &'static str {
    "sample"
}

#[test]
fn uri_escapes_of_a_frame_place_are_decoded() {
    let encoded = format!("%3Creflect.lang%28NativeAccessor.{}%29", extension());
    assert_eq!(
        decode_uri(&encoded),
        format!("<reflect.lang(NativeAccessor.{})", extension())
    );
    assert_eq!(
        decode_uri("C:/work/My%20Project/Main.sample"),
        "C:/work/My Project/Main.sample"
    );
    assert_eq!(decode_uri("%3Cinit%3E"), "<init>");
    assert_eq!(
        decode_uri("C:/%D0%9F%D1%80%D0%B8%D0%BC%D0%B5%D1%80.sample"),
        "C:/Пример.sample"
    );
    // A malformed escape stays as it came.
    assert_eq!(decode_uri("%zz %2"), "%zz %2");
}

#[test]
fn a_class_frame_of_a_jdt_buffer_shows_its_idea_place() {
    let encoded = format!(
        "jdt://contents/runtime.base/runtime.lang.reflect/Method.{0}?=runtime.base%2F\
         runtime.lang.reflect%2FMethod.{0}%3D%3Cruntime.lang.reflect%28Method.{0}",
        extension()
    );
    let place = frame("Method.invoke(Object,Object[])", Some(&encoded), 569);
    assert_eq!(
        frame_place(&place, &[]),
        "invoke:569, Method (runtime.lang.reflect)"
    );

    // The package may come with its separators encoded as well.
    let encoded = format!(
        "jdt://contents/lib-1.0.jar/demo%2Futil/Items.{}?=lib-1.0.jar",
        extension()
    );
    let place = frame("Items.add(Object)", Some(&encoded), 12);
    assert_eq!(frame_place(&place, &[]), "add:12, Items (demo.util)");

    // A name that carries the package of its own keeps it.
    let place = frame(
        "runtime.lang.reflect.Method.invoke",
        Some("jdt://contents/runtime.base"),
        7,
    );
    assert_eq!(
        frame_place(&place, &[]),
        "invoke:7, Method (runtime.lang.reflect)"
    );
}

#[test]
fn a_project_frame_shows_its_idea_place() {
    let root = Path::new("C:/work/demo");
    let roots = [root.join("src")];
    let project = frame(
        "Controller.processFindForm(int,Model)",
        Some("C:/work/demo/src/demo/Controller.sample"),
        107,
    );
    assert_eq!(
        frame_place(&project, &roots),
        "processFindForm:107, Controller (demo)"
    );

    // Without the source roots the name still names the package.
    let project = frame(
        "demo.Main.main",
        Some("C:/work/demo/src/demo/Main.sample"),
        5,
    );
    assert_eq!(frame_place(&project, &[]), "main:5, Main (demo)");
}

#[test]
fn a_frame_of_a_runtime_image_or_an_archive_shows_its_idea_place() {
    let image = frame(
        "Method.invoke0(int)",
        Some("jrt:/runtime.base/runtime/lang/reflect/Method.sample"),
        0,
    );
    assert_eq!(
        frame_place(&image, &[]),
        "invoke0:-1, Method (runtime.lang.reflect)"
    );

    let archive = frame(
        "Lib.run(runtime.util.List)",
        Some("C:/libs/lib-1.0.jar!/org/example/Lib.sample"),
        12,
    );
    assert_eq!(frame_place(&archive, &[]), "run:12, Lib (org.example)");

    let constructor = frame(
        "demo.Outer.%3Cinit%3E",
        Some("C:/libs/demo.jar!/demo/Outer.sample"),
        3,
    );
    assert_eq!(frame_place(&constructor, &[]), "<init>:3, Outer (demo)");
}

#[test]
fn a_native_frame_has_the_line_minus_one() {
    let native = frame(
        "jdk.internal.reflect.NativeMethodAccessorImpl.invoke0(int)",
        None,
        0,
    );
    assert_eq!(
        frame_place(&native, &[]),
        "invoke0:-1, NativeMethodAccessorImpl (jdk.internal.reflect)"
    );
    let unknown = frame("run", None, 0);
    assert_eq!(frame_place(&unknown, &[]), "run:-1");
}
