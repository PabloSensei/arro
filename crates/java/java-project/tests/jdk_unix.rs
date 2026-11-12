//! A JDK of a Unix install: no extension on the programs of `bin`.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use java_project::Jdk;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("jdk-unix-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(path.join("bin")).unwrap();
    path
}

#[test]
fn a_release_file_gives_the_version_and_the_program_names() {
    let home = scratch("home");
    fs::write(home.join("bin").join("javac"), "").unwrap();
    fs::write(
        home.join("release"),
        "IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"21.0.2\"\n",
    )
    .unwrap();

    let found = Jdk::at(&home).unwrap();

    assert_eq!((found.version.as_str(), found.feature), ("21.0.2", 21));
    assert_eq!(found.java(), home.join("bin").join("java"));
    let _ = fs::remove_dir_all(home);
}
