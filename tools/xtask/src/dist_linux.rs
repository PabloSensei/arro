//! The Linux folder and its `tar.gz`: the program, the helper jars, the language tools,
//! the icon, a desktop entry and an optional `install.sh`.
//!
//! The package is built on Linux, where the release binary and the executable bits of
//! `install.sh` come from. Everything that is text is written here, so the layout can be
//! checked from any system.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, dist, helpers, notice};

const BINARY: &str = "arro";
const README: &str = include_str!("../../../dist/README-linux.txt");
const DESKTOP: &str = include_str!("../../../dist/arro.desktop");
const INSTALL: &str = include_str!("../../../dist/install.sh");

pub fn run(root: &Path, out: &Path, tools: Option<&Path>, archive: bool) -> Result {
    let out = dist::absolute(out)?;
    let root = dist::absolute(root)?;
    if out == root {
        return Err("refusing to write the distribution into the workspace root".into());
    }
    let tools = dist::tools_directory(tools)?;
    fs::create_dir_all(&out)?;
    remove_old(&out)?;

    dist::build_release(&root)?;
    let binary = dist::target_directory(&root)?.join("release").join(BINARY);
    if !binary.is_file() {
        return Err(format!(
            "{} was not built; the Linux release is built on Linux",
            binary.display()
        )
        .into());
    }
    let program = out.join(BINARY);
    fs::copy(&binary, &program)?;
    dist::make_executable(&program)?;
    split_debug_info(&program, &dist::default_symbols_dir(&out))?;
    println!("{}", program.display());

    let jacoco = dist::jacoco_lib(&tools)?;
    helpers::run_with(&root, &out.join("helpers"), None, Some(&jacoco))?;
    let java = dist::install_java_tools_linux(&out, &tools)?;
    dist::install_coverage_agent(&out, &tools)?;

    let commit =
        dist::git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let date = dist::git(&root, &["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    dist::write_unix_text(
        &out.join("README.txt"),
        &README
            .replace("{{COMMIT}}", &commit)
            .replace("{{DATE}}", &date),
    )?;
    dist::write_unix_text(
        &out.join("THIRD-PARTY.txt"),
        &linux_paths(&dist::third_party_for(&root, "x86_64-unknown-linux-gnu")?),
    )?;
    let mut text = notice::render(false);
    text.push_str(&linux_paths(&dist::java_notice(&java)));
    dist::write_unix_text(&out.join("NOTICE.txt"), &text)?;
    dist::write_unix_text(
        &java.debug.join("extension").join("EPL-1.0.txt"),
        dist::EPL_1_0,
    )?;
    dist::write_unix_text(&out.join("arro.desktop"), DESKTOP)?;
    fs::copy(
        root.join("crates/app/ide/resources/arro-256.png"),
        out.join("arro-256.png"),
    )?;
    let installer = out.join("install.sh");
    dist::write_unix_text(&installer, INSTALL)?;
    dist::make_executable(&installer)?;

    println!("dist: {} (commit {commit}, {date})", out.display());
    if archive {
        let archive = tar_gz_folder(&out)?;
        println!("{}", archive.display());
    }
    Ok(())
}

/// The texts of the Windows folder with the paths, the binary and the target of the
/// Linux build.
fn linux_paths(text: &str) -> String {
    text.replace("arro.exe", BINARY)
        .replace("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")
        .replace('\\', "/")
}

/// The archive sits next to the folder and holds the folder itself.
fn tar_gz_folder(out: &Path) -> Result<PathBuf> {
    let parent = out.parent().ok_or("the output folder has no parent")?;
    let name = out.file_name().ok_or("the output folder has no name")?;
    let mut archive_name = name.to_os_string();
    archive_name.push(".tar.gz");
    let archive = parent.join(archive_name);
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let mut command = Command::new(dist::tar());
    command
        .args(["-c", "-z", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(parent)
        .arg(name);
    dist::check(&mut command)?;
    Ok(archive)
}

/// Only what this command writes is replaced, as in the Windows folder.
/// Moves the line tables of the release build out of the program into
/// `<symbols>/arro.debug`, the way the Windows release keeps its pdb out of the zip, and
/// leaves a debug link behind. Without `objcopy` the program keeps them: the archive is
/// larger, nothing else changes.
fn split_debug_info(program: &Path, symbols: &Path) -> Result {
    fs::create_dir_all(symbols)?;
    let debug = symbols.join(format!("{BINARY}.debug"));
    let keep = Command::new("objcopy")
        .arg("--only-keep-debug")
        .arg(program)
        .arg(&debug)
        .status();
    match keep {
        Ok(status) if status.success() => {}
        Ok(status) => return Err(format!("objcopy --only-keep-debug failed: {status}").into()),
        Err(error) => {
            println!("objcopy is not available ({error}); the program keeps its debug info");
            return Ok(());
        }
    }
    let mut strip = Command::new("objcopy");
    strip
        .arg("--strip-debug")
        .arg(format!("--add-gnu-debuglink={}", debug.display()))
        .arg(program);
    dist::check(&mut strip)?;
    println!("{}", debug.display());
    Ok(())
}

fn remove_old(out: &Path) -> Result {
    for name in [
        BINARY,
        "README.txt",
        "THIRD-PARTY.txt",
        "NOTICE.txt",
        "arro.desktop",
        "arro-256.png",
        "install.sh",
    ] {
        let file = out.join(name);
        if file.exists() {
            fs::remove_file(file)?;
        }
    }
    for folder in ["helpers", "tools"] {
        let folder = out.join(folder);
        if folder.exists() {
            fs::remove_dir_all(folder)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::TempDir;

    #[test]
    fn the_archive_holds_the_release_folder() {
        let root = TempDir::new("dist-linux");
        let out = root.path().join("arro-0.1.0-linux-x86_64");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("arro"), b"program").unwrap();
        fs::write(out.join("arro.desktop"), b"entry").unwrap();

        let archive = tar_gz_folder(&out).unwrap();
        assert_eq!(archive, root.path().join("arro-0.1.0-linux-x86_64.tar.gz"));
        assert!(archive.is_file());

        let unpacked = root.path().join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        let status = Command::new(dist::tar())
            .args(["-x", "-z", "-f"])
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read(unpacked.join("arro-0.1.0-linux-x86_64/arro")).unwrap(),
            b"program"
        );
        assert_eq!(
            fs::read(unpacked.join("arro-0.1.0-linux-x86_64/arro.desktop")).unwrap(),
            b"entry"
        );
    }

    #[test]
    fn the_paths_of_the_windows_texts_become_linux_ones() {
        let text = "tools\\server.jar, arro.exe, x86_64-pc-windows-msvc";
        assert_eq!(
            linux_paths(text),
            "tools/server.jar, arro, x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn the_desktop_entry_has_a_placeholder_for_the_folder() {
        assert!(DESKTOP.contains("Exec=\"@ARRO_DIR@/arro\""));
        assert!(DESKTOP.contains("Icon=arro"));
        assert!(INSTALL.starts_with("#!/bin/sh"));
    }
}
