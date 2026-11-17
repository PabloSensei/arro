//! The commit of the build, the way `xtask dist` writes it into the distribution README.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Asks cargo to run this script again when the commit changes: the HEAD of this
/// checkout (a worktree has its own), the branch it points to and the packed refs.
fn watch_head() {
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!(
            "cargo:rerun-if-changed={}",
            PathBuf::from(dir).join("HEAD").display()
        );
    }
    if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common = PathBuf::from(common);
        println!(
            "cargo:rerun-if-changed={}",
            common.join("packed-refs").display()
        );
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", common.join(branch).display());
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    watch_head();
    let version = git(&["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_default();
    println!("cargo:rustc-env=IDE_BUILD_VERSION={version}");
    println!("cargo:rustc-env=IDE_BUILD_DATE={date}");
}
