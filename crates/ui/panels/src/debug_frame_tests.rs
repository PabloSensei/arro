//! The unit tests of the place of a frame in the frames list: the percent escapes of a
//! `jrt:` path and the `method:line, Class (package)` form of a library frame.

use std::path::{Path, PathBuf};

use dap_client::Frame;

use super::{decode_uri, frame_place, library_place};

fn frame(name: &str, source: Option<&str>, line: u32) -> Frame {
    Frame {
        id: 1,
        name: name.to_owned(),
        source: source.map(PathBuf::from),
        line,
        column: 0,
    }
}

/// The extension of a source file of a runtime image. The parsing does not depend on it,
/// so the tests use a neutral one.
fn source_extension() -> String {
    "sample".to_owned()
}

#[test]
fn uri_escapes_of_a_frame_place_are_decoded() {
    let encoded = format!(
        "%3Cjdk.internal.reflect%28NativeMethodAccessorImpl.{}%29",
        source_extension()
    );
    assert_eq!(
        decode_uri(&encoded),
        format!(
            "<jdk.internal.reflect(NativeMethodAccessorImpl.{})",
            source_extension()
        )
    );
    assert_eq!(
        decode_uri("C:/work/My%20Project/Main.src"),
        "C:/work/My Project/Main.src"
    );
    assert_eq!(decode_uri("%3Cinit%3E"), "<init>");
    assert_eq!(
        decode_uri("C:/%D0%9F%D1%80%D0%B8%D0%BC%D0%B5%D1%80.src"),
        "C:/Пример.src"
    );
    // A malformed escape stays as it came.
    assert_eq!(decode_uri("%zz %2"), "%zz %2");
}

#[test]
fn a_library_frame_names_its_method_class_and_package() {
    assert_eq!(
        library_place(
            "jdk.internal.reflect.NativeMethodAccessorImpl.invoke0(int, jdk.internal.reflect.Caller)",
            0
        ),
        "invoke0:-1, NativeMethodAccessorImpl (jdk.internal.reflect)"
    );
    assert_eq!(
        library_place(
            "jdk.internal.reflect.Method.invoke(int, jdk.internal.reflect.Caller)",
            42
        ),
        "invoke:42, Method (jdk.internal.reflect)"
    );
    assert_eq!(library_place("demo.Main.run", 7), "run:7, Main (demo)");
    // The parameter list may hold dots of its own.
    assert_eq!(
        library_place("org.example.Lib.run(runtime.util.List, int)", 10),
        "run:10, Lib (org.example)"
    );
    assert_eq!(
        library_place("demo.Outer.<init>", 3),
        "<init>:3, Outer (demo)"
    );
    assert_eq!(library_place("run", 0), "run:-1");
}

#[test]
fn a_frame_of_a_runtime_image_or_an_archive_shows_its_idea_place() {
    let root = Path::new("C:/work/demo");
    let image = frame(
        "jdk.internal.reflect.Method.invoke0(int)",
        Some(&format!(
            "jrt:/runtime.base/%3Cjdk.internal.reflect%28NativeMethodAccessorImpl.{}%29",
            source_extension()
        )),
        0,
    );
    assert_eq!(
        frame_place(&image, root),
        "invoke0:-1, Method (jdk.internal.reflect)"
    );
    let archive = frame(
        "org.example.Lib.run(runtime.util.List)",
        Some("C:/libs/spring-core-6.1.0.jar!/org/example/Lib.class"),
        12,
    );
    assert_eq!(frame_place(&archive, root), "run:12, Lib (org.example)");
    let constructor = frame(
        "demo.Outer.%3Cinit%3E",
        Some("C:/libs/demo.jar!/demo/Outer.class"),
        3,
    );
    assert_eq!(frame_place(&constructor, root), "<init>:3, Outer (demo)");
}

#[test]
fn a_project_frame_shows_its_decoded_file_and_line() {
    let root = Path::new("C:/work/demo");
    let project = frame("demo.Main.main", Some("C:/work/demo/src/My%20File.src"), 5);
    assert_eq!(frame_place(&project, root), "demo.Main.main  My File.src:5");
    let unknown = frame("run", None, 0);
    assert_eq!(frame_place(&unknown, root), "run  ");
}
