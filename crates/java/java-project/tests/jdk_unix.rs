//! A JDK of a Unix install: no extension on the programs of `bin`.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use java_project::Jdk;

fn home(name: &str) -> PathBuf {
    let path = test_support::scratch(&format!("jdk-unix-{name}"));
    fs::create_dir_all(path.join("bin")).unwrap();
    path
}

#[test]
fn a_release_file_gives_the_version_and_the_program_names() {
    let home = home("home");
    fs::write(home.join("bin").join("javac"), "").unwrap();
    fs::write(
        home.join("release"),
        "IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"21.0.2\"\n",
    )
    .unwrap();

    let found = Jdk::at(&home).unwrap();

    assert_eq!((found.version.as_str(), found.feature), ("21.0.2", 21));
    assert_eq!(found.java(), home.join("bin").join("java"));
}
