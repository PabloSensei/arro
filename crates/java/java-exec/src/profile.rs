//! Profile runs on Flight Recorder: the recording flag of a run, the helper that turns
//! a recording into the tree of the window, and the recordings the window lists. The
//! parser and the command of the helper live with the build tool, which also records the
//! JVMs of its tasks.

#[allow(unused_imports)] // The tests read them through `super::*`.
pub use java_project::profile::{
    MAX_RECORDINGS, MAX_SAMPLES, PROFILE_FOLDER, parse_reader, parse_report, recording_file,
    recording_flag, recordings, report, report_of, run_recordings, snapshot, supported,
};
#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use java_project::Jdk;
    use project_model::{Module, ProjectModel, Unit};

    use super::*;

    const STACKS: &str = r#"{
      "recording": {
        "events": [
          {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": [
            {"method": {"type": {"name": "demo.Counter", "package": {"name": "demo"}},
                        "name": "bump", "descriptor": "()I"}, "lineNumber": 7},
            {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                        "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 12}
          ]}}},
          {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": [
            {"method": {"type": {"name": "demo.Counter", "package": {"name": "demo"}},
                        "name": "bump", "descriptor": "()I"}, "lineNumber": 7},
            {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                        "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 12}
          ]}}},
          {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": [
            {"method": {"type": {"name": "demo.Counter", "package": {"name": "demo"}},
                        "name": "unused", "descriptor": "()I"}, "lineNumber": 16},
            {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                        "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 13}
          ]}}}
        ]
      }
    }"#;

    fn model(root: &Path) -> ProjectModel {
        ProjectModel {
            root: root.to_owned(),
            ..ProjectModel::default()
        }
    }

    #[test]
    fn the_tree_merges_equal_stacks_and_counts_own_and_total_samples() {
        let snapshot =
            parse_reader(STACKS.as_bytes(), &model(Path::new("C:/p")), MAX_SAMPLES).unwrap();

        assert_eq!(snapshot.samples, 3);
        assert!(!snapshot.truncated);
        assert_eq!(snapshot.roots.len(), 1, "both stacks start in main");
        let main = &snapshot.roots[0];
        assert_eq!(main.frame.name(), "demo.Main.main");
        assert_eq!((main.own, main.total), (0, 3));
        assert_eq!(main.children.len(), 2);
        let bump = main.child("demo.Counter.bump").unwrap();
        assert_eq!((bump.own, bump.total), (2, 2));
        let unused = main.child("demo.Counter.unused").unwrap();
        assert_eq!((unused.own, unused.total), (1, 1));
        assert_eq!(bump.frame.line, Some(7));
    }

    #[test]
    fn the_limit_keeps_the_first_samples_and_says_the_rest_were_dropped() {
        let snapshot = parse_reader(STACKS.as_bytes(), &model(Path::new("C:/p")), 2).unwrap();

        assert_eq!(snapshot.samples, 2);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.roots[0].total, 2);
    }

    #[test]
    fn a_frame_of_the_project_gets_its_source_and_a_library_frame_does_not() {
        let root = test_support::scratch("profile-sources");
        let sources = root.join("src");
        fs::create_dir_all(sources.join("demo")).unwrap();
        fs::write(sources.join("demo/Main.java"), "class Main {}\n").unwrap();
        let model = ProjectModel {
            root: root.clone(),
            modules: vec![Module {
                units: vec![Unit {
                    source_roots: vec![sources.clone()],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        let snapshot = parse_reader(STACKS.as_bytes(), &model, MAX_SAMPLES).unwrap();

        let frame = &snapshot.roots[0].frame;
        assert_eq!(frame.source, Some(sources.join("demo/Main.java")));
        assert!(frame.is_own());
        assert_eq!(frame.package, "demo");
    }

    #[test]
    fn an_event_without_a_stack_is_not_a_sample() {
        let json = r#"{"recording": {"events": [
            {"type": "jdk.ExecutionSample", "values": {}},
            {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": []}}}
        ]}}"#;

        let snapshot =
            parse_reader(json.as_bytes(), &model(Path::new("C:/p")), MAX_SAMPLES).unwrap();

        assert_eq!(snapshot.samples, 0);
        assert!(snapshot.roots.is_empty());
    }

    #[test]
    fn the_recording_flag_names_the_file_and_writes_it_at_exit() {
        let flag = recording_flag(Path::new("C:/cache/profiler/App-1.jfr"));

        assert!(flag.starts_with("-XX:StartFlightRecording=filename=\""));
        assert!(flag.contains("App-1.jfr"));
        assert!(flag.contains("settings=profile"));
        assert!(flag.contains("dumponexit=true"));
    }

    #[test]
    fn only_the_newest_recordings_stay_in_the_folder() {
        let root = test_support::scratch("profile-recordings");
        let directory = root.join(PROFILE_FOLDER);
        fs::create_dir_all(&directory).unwrap();
        for index in 0..(MAX_RECORDINGS + 2) {
            fs::write(directory.join(format!("old-{index}.jfr")), b"jfr").unwrap();
        }

        let file = recording_file(&root, "My App").unwrap();

        assert_eq!(file.parent(), Some(directory.as_path()));
        assert!(
            file.file_stem()
                .unwrap()
                .to_string_lossy()
                .starts_with("My-App-")
        );
        fs::write(&file, b"jfr").unwrap();
        let left = fs::read_dir(&directory).unwrap().count();
        assert_eq!(left, MAX_RECORDINGS, "the folder keeps the newest only");
        assert_eq!(recordings(&root).len(), MAX_RECORDINGS);
    }

    #[test]
    fn a_jdk_8_before_the_update_has_no_flight_recorder() {
        let jdk = |feature: u32, version: &str| Jdk {
            home: PathBuf::from("C:/jdk"),
            version: version.to_owned(),
            feature,
        };

        assert!(!supported(&jdk(8, "1.8.0_252")));
        assert!(supported(&jdk(8, "1.8.0_492")));
        assert!(supported(&jdk(17, "17.0.20.1")));
    }
}
