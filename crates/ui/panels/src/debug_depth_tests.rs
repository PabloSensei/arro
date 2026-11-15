//! The unit tests of the deeper debugger: the frames of libraries, the text of the
//! renderer settings and the inline watches of the saved state.

use std::path::{Path, PathBuf};

use dap_client::Frame;

use super::{
    InlineWatchSetting, SavedState, artifact_name, is_library_frame, library_group_name,
    parse_renderers,
};

fn frame(name: &str, source: Option<&str>) -> Frame {
    Frame {
        id: 1,
        name: name.to_owned(),
        source: source.map(PathBuf::from),
        line: 0,
        column: 0,
    }
}

#[test]
fn a_frame_outside_the_project_is_a_library_frame() {
    let root = Path::new("C:/work/demo");
    assert!(!is_library_frame(
        &frame("demo.Main.main", Some("C:/work/demo/src/Main.txt")),
        root
    ));
    assert!(is_library_frame(
        &frame("org.example.Lib.run", Some("C:/libs/lib-1.0.jar")),
        root
    ));
    assert!(is_library_frame(
        &frame(
            "runtime.util.Items.add",
            Some("jrt:/runtime.base/runtime/util/Items.txt")
        ),
        root
    ));
    // The adapter did not say where the frame is; it stays.
    assert!(!is_library_frame(&frame("run", None), root));
}

#[test]
fn a_library_group_is_named_by_its_module_or_artifact() {
    assert_eq!(
        library_group_name(&frame(
            "runtime.util.Items.add",
            Some("jrt:/runtime.base/runtime/util/Items.txt")
        )),
        "runtime.base"
    );
    assert_eq!(
        library_group_name(&frame(
            "org.example.Lib.run",
            Some("C:/Users/pavel/.m2/spring-core-6.1.0.jar")
        )),
        "spring-core"
    );
    assert_eq!(
        library_group_name(&frame(
            "org.example.Lib.run",
            Some("C:/libs/commons-io-2.19.0.jar!/org/example/Lib.class")
        )),
        "commons-io"
    );
    assert_eq!(
        library_group_name(&frame("org.example.Lib.run", Some("C:/src/Lib.txt"))),
        "org.example"
    );
    assert_eq!(artifact_name("guava"), "guava");
}

#[test]
fn renderer_rules_come_from_the_settings_text() {
    let rules = parse_renderers(
        "demo.Money = amount + \" \" + currency | amount; demo.Item = name() ; broken ; = x",
    );
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].type_name, "demo.Money");
    assert_eq!(rules[0].display, "amount + \" \" + currency");
    assert_eq!(rules[0].children.as_deref(), Some("amount"));
    assert_eq!(rules[1].type_name, "demo.Item");
    assert_eq!(rules[1].display, "name()");
    assert_eq!(rules[1].children, None);
    assert!(parse_renderers("").is_empty());
}

#[test]
fn inline_watches_survive_the_saved_state() {
    let saved = SavedState {
        inline_watches: vec![InlineWatchSetting {
            file: PathBuf::from("C:/demo/Main.txt"),
            line: 7,
            expression: "items.size()".into(),
        }],
        ..SavedState::default()
    };
    let json = serde_json::to_string(&saved).unwrap();
    let back: SavedState = serde_json::from_str(&json).unwrap();
    assert_eq!(back.inline_watches[0].line, 7);
    assert_eq!(back.inline_watches[0].expression, "items.size()");
}
