#[cfg(all(test, windows))]
mod app_resources_tests;
mod budgets;
mod ci;
mod conpty;
mod dist;
mod helpers;
mod icons;
mod idea_keymap;
mod layers;
mod notice;
mod release;
mod scrollbars;
mod spacing;
mod symbolize;
#[cfg(test)]
mod third_party_tests;
mod tooltips;

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  ci                         fmt, clippy, tests, cargo deny, check-layers, check-budgets,
                             check-spacing, check-tooltips, check-scrollbars
  check-layers [--root DIR]  dependency graph and forbidden words, ARCHITECTURE.md section 4
  check-budgets              budgets.toml structure
  check-spacing              padding, margins and gaps in the UI crates are on the spacing scale
  check-tooltips             every icon-only button in the UI crates names itself in a tooltip
  check-scrollbars           every scrolled area in the UI crates has scrollbars or is exempt
  build-helpers --out DIR [--maven-lib DIR]
                             the helper jars that run in project JVMs, needs a JDK
  dist --out DIR [--symbols DIR] [--tools DIR] [--zip]
                             release Arro, helpers, jdtls, java-debug, README, run-arro.cmd
                             and THIRD-PARTY.txt; the jdtls and java-debug archives come
                             from --tools DIR, $IDE_CORPUS/tools by default; arro.pdb and
                             arro.exe go to DIR-symbols/<commit> by default
  symbolize LOG [--symbols DIR]
                             function and file:line for the arro.exe frames of a panic log;
                             DIR is target/symbols by default
  release-assets --out DIR   zip a ready dist folder and write its .sha256 next to it
  fetch-conpty               download the pinned ConPTY package into crates/services/terminal/vendor
  idea-keymap XML JSON       IntelliJ IDEA $default.xml to the keymap crate's JSON
  icons --lucide DIR         generate assets/icons from the Lucide sources in DIR";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let root = workspace_root();
    let result = match args.as_slice() {
        ["ci"] => ci::run(&root),
        ["check-layers"] => layers::run(&root),
        ["check-layers", "--root", other] => layers::run(Path::new(other)),
        ["check-budgets"] => budgets::run(&root),
        ["check-spacing"] => spacing::run(&root),
        ["check-tooltips"] => tooltips::run(&root),
        ["check-scrollbars"] => scrollbars::run(&root),
        ["fetch-conpty"] => conpty::fetch(&root),
        ["idea-keymap", xml, json] => idea_keymap::run(Path::new(xml), Path::new(json)),
        ["build-helpers", "--out", out] => helpers::run(&root, Path::new(out), None),
        ["build-helpers", "--out", out, "--maven-lib", lib] => {
            helpers::run(&root, Path::new(out), Some(Path::new(lib)))
        }
        ["dist", args @ ..] => dist_command(&root, args),
        ["symbolize", args @ ..] => symbolize_command(&root, args),
        ["release-assets", "--out", out] => release::run(Path::new(out)),
        ["icons", "--lucide", dir] => icons::run(&root, Path::new(dir)),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--out DIR`, an optional `--tools DIR` and `--zip`, in any order.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `--out DIR`, `--symbols DIR`, `--tools DIR` and `--zip` in any order.
fn dist_command(root: &Path, args: &[&str]) -> Result {
    let mut out = None;
    let mut symbols = None;
    let mut tools = None;
    let mut zip = false;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--zip" => zip = true,
            "--out" => {
                index += 1;
                out = Some(Path::new(args.get(index).ok_or("--out needs a directory")?));
            }
            "--symbols" => {
                index += 1;
                symbols = Some(Path::new(
                    args.get(index).ok_or("--symbols needs a directory")?,
                ));
            }
            "--tools" => {
                index += 1;
                tools = Some(Path::new(
                    args.get(index).ok_or("--tools needs a directory")?,
                ));
            }
            other => return Err(format!("unknown dist option: {other}").into()),
        }
        index += 1;
    }
    let out = out.ok_or("dist needs --out DIR")?;
    dist::run(root, out, symbols, tools, zip)
}

/// `LOG` and an optional `--symbols DIR`.
fn symbolize_command(root: &Path, args: &[&str]) -> Result {
    let mut log = None;
    let mut symbols = None;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--symbols" => {
                index += 1;
                symbols = Some(Path::new(
                    args.get(index).ok_or("--symbols needs a directory")?,
                ));
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown symbolize option: {other}").into());
            }
            other => log = Some(Path::new(other)),
        }
        index += 1;
    }
    let log = log.ok_or("symbolize needs a log file")?;
    symbolize::run(root, log, symbols)
}

/// The cargo that started xtask, so nested runs stay on the pinned toolchain.
fn cargo() -> std::ffi::OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}
