//! A folder left by a partly removed JDK is not a JDK, wherever the machine keeps it.
//!
//! The tests make their own folders: no registry and no standard place of the machine
//! is read.

use std::fs;
use std::path::PathBuf;

use base::program::executable;
use java_project::Jdk;

fn folder(name: &str) -> PathBuf {
    let home = test_support::scratch(&format!("jdk-removed-{name}"));
    fs::create_dir_all(home.join("bin")).unwrap();
    fs::write(home.join("release"), "JAVA_VERSION=\"25.0.3\"\n").unwrap();
    home
}

#[test]
fn a_toolchain_without_the_java_program_is_not_a_jdk() {
    let home = folder("runtime-only");
    fs::write(home.join("bin").join(executable("javac")), "").unwrap();
    // A partial removal leaves the folder and perhaps the compiler behind, but the
    // build tool cannot start a JVM with it.
    assert_eq!(Jdk::at(&home), None);
}

#[test]
fn a_toolchain_without_the_compiler_is_not_a_jdk() {
    let home = folder("java-only");
    fs::write(home.join("bin").join(executable("java")), "").unwrap();
    assert_eq!(Jdk::at(&home), None);
}

#[test]
fn a_complete_toolchain_is_a_jdk() {
    let home = folder("complete");
    fs::write(home.join("bin").join(executable("javac")), "").unwrap();
    fs::write(home.join("bin").join(executable("java")), "").unwrap();
    let jdk = Jdk::at(&home).expect("both programs are there");
    assert_eq!(jdk.feature, 25);
    assert_eq!(jdk.java(), home.join("bin").join(executable("java")));
}
