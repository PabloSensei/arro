//! Runs the real cargo-deny with the repository's deny.toml against small fixtures.

mod support;

use std::path::Path;
use std::process::{Command, Output};

use support::Fixture;

fn deny_licenses(fixture: &Fixture) -> Output {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deny.toml");
    Command::new(env!("CARGO"))
        .args(["deny", "--manifest-path"])
        .arg(fixture.root.join("Cargo.toml"))
        .arg("--config")
        .arg(config)
        .args(["check", "licenses"])
        .output()
        .expect("cargo-deny is required: cargo install cargo-deny --locked")
}

fn app_with_dependency(name: &str, dependency: &str, license: &str) -> Fixture {
    let path = format!("vendor/{dependency}");
    Fixture::new(name)
        .krate_with(&path, &format!("license = \"{license}\""), &[])
        .krate_with("crates/app/app", "publish = false", &[&path])
}

fn assert_rejected(fixture: &Fixture, license: &str) {
    let output = deny_licenses(fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{license} was accepted:\n{stderr}"
    );
    assert!(stderr.contains("rejected"), "unexpected failure:\n{stderr}");
    assert!(stderr.contains(license), "unexpected failure:\n{stderr}");
}

fn assert_accepted(fixture: &Fixture) {
    let output = deny_licenses(fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "expected a pass:\n{stderr}");
}

#[test]
fn allowlisted_license_passes() {
    assert_accepted(&app_with_dependency("deny-mit", "good-dep", "MIT"));
}

#[test]
fn cc0_passes_only_for_listed_crates() {
    let listed = app_with_dependency("deny-cc0-listed", "notify", "CC0-1.0");
    assert_accepted(&listed);

    let unlisted = app_with_dependency("deny-cc0-unlisted", "other-crate", "CC0-1.0");
    assert_rejected(&unlisted, "CC0-1.0");
}

#[test]
fn gpl_dependency_is_rejected() {
    let fixture = app_with_dependency("deny-gpl", "bad-dep", "GPL-3.0-or-later");
    assert_rejected(&fixture, "GPL-3.0-or-later");
}

#[test]
fn agpl_dependency_is_rejected() {
    let fixture = app_with_dependency("deny-agpl", "bad-dep", "AGPL-3.0-only");
    assert_rejected(&fixture, "AGPL-3.0-only");
}

#[test]
fn mpl_passes_only_for_listed_crates() {
    let listed = app_with_dependency("deny-mpl-listed", "nucleo-matcher", "MPL-2.0");
    assert_accepted(&listed);

    let unlisted = app_with_dependency("deny-mpl-unlisted", "other-crate", "MPL-2.0");
    assert_rejected(&unlisted, "MPL-2.0");
}
