//! The macOS application bundle: `Arro.app` with its `Info.plist`, its icon, the binary,
//! the language tools, the notices, an ad-hoc signature and the zip around it.
//!
//! The bundle is built on a Mac: `cargo build` without a target triple produces the binary
//! of the machine, `codesign` signs it and `ditto` writes the zip. Everything that is text
//! is written here, so the layout and the files can be checked from any system.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, dist, notice};

const APP: &str = "Arro.app";
/// The name of the program inside the bundle and the one the Finder shows.
const BINARY: &str = "arro";
const DISPLAY_NAME: &str = "Arro";
/// A name of ours, from the domain the project is published under.
const BUNDLE_ID: &str = "life.pablosensei.arro";
const README: &str = include_str!("../../../dist/README-macos.txt");

pub fn run(root: &Path, out: &Path, tools: Option<&Path>, zip: bool) -> Result {
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
        return Err(format!("{} was not built", binary.display()).into());
    }

    let app = out.join(APP);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let program = macos.join(BINARY);
    fs::copy(&binary, &program)?;
    make_executable(&program)?;
    println!("{}", program.display());

    let version = env!("CARGO_PKG_VERSION");
    let commit = git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let date = git(&root, &["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    dist::write_text(
        &contents.join("Info.plist"),
        &info_plist(version, &commit, &date),
    )?;
    fs::write(resources.join(format!("{DISPLAY_NAME}.icns")), icon(&root)?)?;

    let java = dist::install_java_tools_macos(&resources, &tools)?;
    let triple = format!("{}-apple-darwin", env::consts::ARCH);
    dist::write_text(
        &resources.join("README.txt"),
        &README
            .replace("{{COMMIT}}", &commit)
            .replace("{{DATE}}", &date),
    )?;
    dist::write_text(
        &resources.join("THIRD-PARTY.txt"),
        &macos_paths(&dist::third_party_for(&root, &triple)?, &triple),
    )?;
    let mut text = notice::render(false);
    text.push_str(&macos_paths(&dist::java_notice(&java), &triple));
    dist::write_text(&resources.join("NOTICE.txt"), &text)?;
    dist::write_text(
        &java.debug.join("extension").join("EPL-1.0.txt"),
        dist::EPL_1_0,
    )?;

    sign(&app)?;
    println!("dist: {} (commit {commit}, {date})", app.display());
    if zip {
        let archive = zip_bundle(&app)?;
        println!("{}", archive.display());
    }
    Ok(())
}

/// The plist the Finder, `open` and Gatekeeper read. `CFBundleVersion` is the commit and
/// the date, because a bundle of the same version must still differ between builds.
fn info_plist(version: &str, commit: &str, date: &str) -> String {
    let mut text = String::new();
    text.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    text.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ");
    text.push_str("\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    text.push_str("<plist version=\"1.0\">\n<dict>\n");
    for (key, value) in [
        ("CFBundleDevelopmentRegion", "en"),
        ("CFBundleDisplayName", DISPLAY_NAME),
        ("CFBundleExecutable", BINARY),
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundleName", DISPLAY_NAME),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", version),
        ("CFBundleVersion", version),
        (
            "LSApplicationCategoryType",
            "public.app-category.developer-tools",
        ),
        ("LSMinimumSystemVersion", "11.0"),
        ("NSHighResolutionCapable", "true"),
        ("NSPrincipalClass", "NSApplication"),
    ] {
        text.push_str(&format!("\t<key>{key}</key>\n"));
        if value == "true" {
            text.push_str("\t<true/>\n");
        } else {
            text.push_str(&format!("\t<string>{}</string>\n", escape(value)));
        }
    }
    text.push_str(&format!(
        "\t<key>CFBundleIconFile</key>\n\t<string>{DISPLAY_NAME}</string>\n"
    ));
    text.push_str(&format!(
        "\t<key>ArroBuild</key>\n\t<string>{}</string>\n",
        escape(&format!("{commit} {date}"))
    ));
    text.push_str("</dict>\n</plist>\n");
    text
}

/// `&`, `<` and `>` are not allowed in a plist value as they are.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The icon: the PNG renders of `arro.svg` that the Windows build carries, in an ICNS
/// container. macOS reads PNG chunks in an ICNS since 10.7 and scales them itself.
fn icon(root: &Path) -> Result<Vec<u8>> {
    let resources = root.join("crates/app/ide/resources");
    let small = fs::read(resources.join("arro-16.png"))?;
    let large = fs::read(resources.join("arro-256.png"))?;
    Ok(icon_family(&[(*b"icp4", &small), (*b"ic08", &large)]))
}

/// An ICNS file: the magic, the length of everything, then chunks of a kind, a length and
/// the bytes of the picture.
fn icon_family(chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, bytes) in chunks {
        body.extend_from_slice(kind);
        body.extend_from_slice(&(bytes.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(bytes);
    }
    let mut file = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"icns");
    file.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    file.extend_from_slice(&body);
    file
}

/// The texts of the Windows zip with the paths, the binary and the platform of the bundle.
fn macos_paths(text: &str, triple: &str) -> String {
    text.replace("arro.exe", BINARY)
        .replace("x86_64-pc-windows-msvc", triple)
        .replace('\\', "/")
}

/// Ad-hoc signing: the bundle is not notarized, so the first start needs the consent of
/// the user, which the README explains.
fn sign(app: &Path) -> Result {
    let mut command = Command::new("codesign");
    command.args(["--force", "--sign", "-"]).arg(app);
    check(&mut command)
}

/// `ditto` keeps the bundle together, unlike the zip of a generic archiver.
fn zip_bundle(app: &Path) -> Result<PathBuf> {
    let parent = app.parent().ok_or("the bundle has no parent folder")?;
    let archive = parent.join(format!("{APP}.zip"));
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let mut command = Command::new("ditto");
    command
        .args(["-c", "-k", "--keepParent"])
        .arg(app)
        .arg(&archive);
    check(&mut command)?;
    Ok(archive)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(windows)]
fn make_executable(_path: &Path) -> Result {
    // The bundle is built on a Mac; a copy on Windows keeps the attributes it came with.
    Ok(())
}

/// Only what this command writes is replaced, as in the Windows folder.
fn remove_old(out: &Path) -> Result {
    for name in [APP, "Arro.app.zip"] {
        let path = out.join(name);
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn check(command: &mut Command) -> Result {
    let status = command.status()?;
    if status.success() {
        return Ok(());
    }
    Err(format!("{:?} failed: {status}", command.get_program()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_the_program_and_the_build() {
        let text = info_plist("0.1.4", "abc1234", "2026-11-12");
        assert!(text.starts_with("<?xml version=\"1.0\""));
        assert!(text.contains("<key>CFBundleExecutable</key>\n\t<string>arro</string>"));
        assert!(text.contains("<key>CFBundleIdentifier</key>\n\t<string>life.pablosensei.arro"));
        assert!(text.contains("<key>CFBundleIconFile</key>\n\t<string>Arro</string>"));
        assert!(text.contains("<string>abc1234 2026-11-12</string>"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
    }

    #[test]
    fn a_plist_value_is_escaped() {
        assert_eq!(escape("a & b <c>"), "a &amp; b &lt;c&gt;");
    }

    #[test]
    fn the_icon_is_an_icns_of_png_chunks() {
        let small = b"png16";
        let large = b"png256";
        let file = icon_family(&[(*b"icp4", small.as_slice()), (*b"ic08", large)]);
        assert_eq!(&file[..4], b"icns");
        let length = u32::from_be_bytes(file[4..8].try_into().unwrap()) as usize;
        assert_eq!(length, file.len());
        // The first chunk: kind, length of the chunk, then the pixels.
        assert_eq!(&file[8..12], b"icp4");
        let chunk = u32::from_be_bytes(file[12..16].try_into().unwrap()) as usize;
        assert_eq!(chunk, small.len() + 8);
        assert_eq!(&file[16..16 + small.len()], small);
        assert_eq!(&file[16 + small.len()..16 + small.len() + 4], b"ic08");
    }

    #[test]
    fn the_windows_texts_get_the_paths_of_the_bundle() {
        let text = "in tools\\jdtls-1.63.0 and arro.exe for x86_64-pc-windows-msvc";
        assert_eq!(
            macos_paths(text, "aarch64-apple-darwin"),
            "in tools/jdtls-1.63.0 and arro for aarch64-apple-darwin"
        );
    }
}
