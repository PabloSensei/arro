use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::{Result, budgets, cargo, fields, layers, scrollbars, spacing, tooltips};

const FMT: &[&str] = &["fmt", "--all", "--check"];
const CLIPPY: &[&str] = &[
    "clippy",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--",
    "-D",
    "warnings",
];
// Windows cannot relink the running xtask.exe, and workspace-wide feature
// unification could force exactly that. xtask is tested alone, with the
// same feature set `cargo xtask` was built with.
const TEST_OTHERS: &[&str] = &["test", "--workspace", "--exclude", "xtask"];
const TEST_XTASK: &[&str] = &["test", "--package", "xtask"];
const DENY: &[&str] = &["deny", "--locked", "check"];

pub fn run(root: &Path) -> Result {
    let mut steps = vec![FMT, CLIPPY];
    // Cargo refuses a test run that excludes every package.
    if has_other_members(root)? {
        steps.push(TEST_OTHERS);
    }
    steps.extend([TEST_XTASK, DENY]);

    for step in steps {
        println!("==> cargo {}", step.join(" "));
        // Tests press every button; links they open are recorded, not sent to a browser.
        let status = Command::new(cargo())
            .args(step)
            .current_dir(root)
            .env("ARRO_NO_BROWSER", "1")
            .status()?;
        if !status.success() {
            return Err(format!("cargo {} failed", step.join(" ")).into());
        }
    }
    println!("==> check-layers");
    layers::run(root)?;
    println!("==> check-budgets");
    budgets::run(root)?;
    println!("==> check-spacing");
    spacing::run(root)?;
    println!("==> check-tooltips");
    tooltips::run(root)?;
    println!("==> check-scrollbars");
    scrollbars::run(root)?;
    println!("==> check-fields");
    fields::run(root)
}

/// The checks that make sense on a Unix machine: no Windows formatting of line endings,
/// no `cargo deny` target of another system, but the same lints and the same tests.
pub fn run_linux(root: &Path) -> Result {
    let mut steps = vec![CLIPPY];
    if has_other_members(root)? {
        steps.push(TEST_OTHERS);
    }
    steps.push(TEST_XTASK);

    for step in steps {
        println!("==> cargo {}", step.join(" "));
        let status = Command::new(cargo())
            .args(step)
            .current_dir(root)
            .env("ARRO_NO_BROWSER", "1")
            .status()?;
        if !status.success() {
            return Err(format!("cargo {} failed", step.join(" ")).into());
        }
    }
    println!("==> check-layers");
    layers::run(root)
}

fn has_other_members(root: &Path) -> Result<bool> {
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()?;
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let members = metadata["packages"].as_array().map_or(0, Vec::len);
    Ok(members > 1)
}
