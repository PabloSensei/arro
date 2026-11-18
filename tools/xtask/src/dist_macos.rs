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

use crate::{Result, dist, helpers, notice};

const APP: &str = "Arro.app";
/// The name of the program inside the bundle and the one the Finder shows.
const BINARY: &str = "arro";
const DISPLAY_NAME: &str = "Arro";
/// A name of ours, from the domain the project is published under.
const BUNDLE_ID: &str = "life.pablosensei.arro";
/// The disk image carries a shortcut so the bundle can be dragged into Applications.
#[cfg(unix)]
const APPLICATIONS: &str = "/Applications";
const README: &str = include_str!("../../../dist/README-macos.txt");

pub fn run(root: &Path, out: &Path, tools: Option<&Path>, zip: bool, dmg: bool) -> Result {
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
    // The shim of MCP for the agents outside the window: Contents/Resources/arro-mcp,
    // the folder `resources_dir` names for a program of a bundle.
    let shim = dist::mcp_binary(&root)?;
    let copied = resources.join(dist::mcp_file_name());
    fs::copy(&shim, &copied)?;
    make_executable(&copied)?;
    println!("{}", copied.display());

    let version = env!("CARGO_PKG_VERSION");
    let commit = git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let date = git(&root, &["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    dist::write_text(
        &contents.join("Info.plist"),
        &info_plist(version, &commit, &date),
    )?;
    fs::write(resources.join(format!("{DISPLAY_NAME}.icns")), icon(&root)?)?;

    // The helpers of Maven, the test listener and the coverage report live in
    // Contents/Resources/helpers, the tools in Contents/Resources/tools.
    let jacoco = dist::jacoco_lib(&tools)?;
    helpers::run_with(&root, &resources.join("helpers"), None, Some(&jacoco))?;
    let java = dist::install_java_tools_macos(&resources, &tools)?;
    dist::install_coverage_agent(&resources, &tools)?;
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
    dist::write_text(&resources.join("LICENSE.txt"), dist::PROJECT_LICENSE)?;
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
    if dmg {
        let image = dmg_image(&app)?;
        println!("{}", image.display());
    }
    Ok(())
}

/// The disk image: the bundle and a link to `/Applications` in a folder `hdiutil`
/// compresses, so the Finder shows what to drag where. Someone who downloads the zip
/// keeps using it: the updater reads the zip.
fn dmg_image(app: &Path) -> Result<PathBuf> {
    let parent = app.parent().ok_or("the bundle has no parent folder")?;
    let archive = parent.join(dmg_name(env!("CARGO_PKG_VERSION"), env::consts::ARCH)?);
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let stage = parent.join(format!(".arro-dmg-{}", std::process::id()));
    let result = stage_for_dmg(app, &stage).and_then(|()| run_hdiutil(&stage, &archive));
    let _ = fs::remove_dir_all(&stage);
    if result.is_err() {
        // A failed run must not leave an image that a later step may attach.
        let _ = fs::remove_file(&archive);
    }
    result?;
    Ok(archive)
}

/// `arro-0.1.16-macos-aarch64.dmg`, the same platform suffix as the zip.
fn dmg_name(version: &str, arch: &str) -> Result<String> {
    let platform = match arch {
        "aarch64" => "macos-aarch64",
        "x86_64" => "macos-x86_64",
        other => return Err(format!("no macOS release for the {other} architecture").into()),
    };
    Ok(format!("arro-{version}-{platform}.dmg"))
}

/// The folder `hdiutil` reads: a copy of the bundle and the shortcut. The copy keeps
/// the modes and the links of the bundle, so the signature survives, and needs no tool
/// that the check of `hdiutil` would not name anyway.
fn stage_for_dmg(app: &Path, stage: &Path) -> Result {
    if stage.exists() {
        fs::remove_dir_all(stage)?;
    }
    fs::create_dir_all(stage)?;
    copy_tree(app, &stage.join(APP))?;
    create_applications_link(&stage.join("Applications"))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result {
    let metadata = fs::symlink_metadata(from)?;
    if metadata.file_type().is_symlink() {
        copy_link(from, to)?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(from, to)?;
    Ok(())
}

#[cfg(unix)]
fn copy_link(from: &Path, to: &Path) -> Result {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_link(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)?;
    Ok(())
}

#[cfg(unix)]
fn create_applications_link(link: &Path) -> Result {
    std::os::unix::fs::symlink(APPLICATIONS, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_applications_link(_link: &Path) -> Result {
    // A disk image is built on macOS; elsewhere only the command is checked.
    Ok(())
}

/// `hdiutil create -volname Arro -srcfolder FOLDER -ov -format UDZO ARCHIVE`.
fn dmg_command(folder: &Path, archive: &Path) -> Command {
    let mut command = Command::new("hdiutil");
    command
        .args(["create", "-volname", DISPLAY_NAME, "-srcfolder"])
        .arg(folder)
        .args(["-ov", "-format", "UDZO"])
        .arg(archive);
    command
}

fn run_hdiutil(folder: &Path, archive: &Path) -> Result {
    let mut command = dmg_command(folder, archive);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("hdiutil create failed: {status}").into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("hdiutil is not available: the disk image is built on macOS".into())
        }
        Err(error) => Err(error.into()),
    }
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
    // No `LSMultipleInstancesProhibited`: Apple documents it for one program in several
    // user sessions of the same Mac (fast user switching), not for a second process of
    // the same user. A program of a project per window is what Arro is; the second start
    // of one state folder hands its path to the running process through the socket.
    text.push_str(&document_types());
    text.push_str("</dict>\n</plist>\n");
    text
}

/// The documents the Finder offers Arro for, and the folders. Every type is an editor
/// and an alternate handler: the default program of the type stays the default, and Arro
/// appears in its "Open With" list.
///
/// macOS has no standard type for a Maven POM or a Gradle script, so those two carry the
/// names this bundle declares for them; `pom.xml` is XML to the system and comes with the
/// XML type too.
fn document_types() -> String {
    const TYPES: &[(&str, &[&str], &[&str])] = &[
        ("Folder", &["public.folder"], &[]),
        ("Java Source", &["com.sun.java-source"], &["java"]),
        (
            "Kotlin Source",
            &["org.jetbrains.kotlin-source"],
            &["kt", "kts"],
        ),
        (
            "Gradle Build Script",
            &["org.gradle.gradle-script"],
            &["gradle"],
        ),
        ("Maven Project", &["org.apache.maven.pom"], &["pom"]),
        (
            "Markdown",
            &["net.daringfireball.markdown"],
            &["md", "markdown"],
        ),
        ("Properties", &["public.plain-text"], &["properties"]),
        ("YAML", &["public.yaml"], &["yml", "yaml"]),
        ("JSON", &["public.json"], &["json"]),
        ("XML", &["public.xml"], &["xml"]),
        ("Text", &["public.plain-text"], &["txt", "text"]),
    ];
    let mut text = String::from("\t<key>CFBundleDocumentTypes</key>\n\t<array>\n");
    for (name, content_types, extensions) in TYPES {
        text.push_str("\t\t<dict>\n");
        text.push_str(&format!(
            "\t\t\t<key>CFBundleTypeName</key>\n\t\t\t<string>{}</string>\n",
            escape(name)
        ));
        text.push_str("\t\t\t<key>CFBundleTypeRole</key>\n\t\t\t<string>Editor</string>\n");
        text.push_str("\t\t\t<key>LSHandlerRank</key>\n\t\t\t<string>Alternate</string>\n");
        text.push_str("\t\t\t<key>LSItemContentTypes</key>\n\t\t\t<array>\n");
        for content_type in *content_types {
            text.push_str(&format!(
                "\t\t\t\t<string>{}</string>\n",
                escape(content_type)
            ));
        }
        text.push_str("\t\t\t</array>\n");
        if !extensions.is_empty() {
            text.push_str("\t\t\t<key>CFBundleTypeExtensions</key>\n\t\t\t<array>\n");
            for extension in *extensions {
                text.push_str(&format!("\t\t\t\t<string>{}</string>\n", escape(extension)));
            }
            text.push_str("\t\t\t</array>\n");
        }
        text.push_str("\t\t</dict>\n");
    }
    text.push_str("\t</array>\n");
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

/// `ditto` keeps the bundle together, unlike the zip of a generic archiver. The name
/// of the archive is the one the updater asks GitHub for.
fn zip_bundle(app: &Path) -> Result<PathBuf> {
    let parent = app.parent().ok_or("the bundle has no parent folder")?;
    let platform = match env::consts::ARCH {
        "aarch64" => "macos-aarch64",
        "x86_64" => "macos-x86_64",
        other => return Err(format!("no macOS release for the {other} architecture").into()),
    };
    let archive = parent.join(format!("arro-{}-{platform}.zip", env!("CARGO_PKG_VERSION")));
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
    for entry in fs::read_dir(out)?.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file()
            && name.starts_with("arro-")
            && (name.ends_with(".zip") || name.ends_with(".dmg"))
        {
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
    use std::path::Path;

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

    /// The Finder offers Arro for the folders and the sources of a project, and leaves
    /// the default program of each type in place: Arro is an alternate handler.
    #[test]
    fn the_plist_opens_the_documents_and_the_folders_of_a_project() {
        let text = info_plist("0.1.4", "abc1234", "2026-11-12");

        assert!(text.contains("<key>CFBundleDocumentTypes</key>"));
        for content_type in [
            "public.folder",
            "com.sun.java-source",
            "org.jetbrains.kotlin-source",
            "org.gradle.gradle-script",
            "org.apache.maven.pom",
            "net.daringfireball.markdown",
            "public.plain-text",
            "public.yaml",
            "public.json",
            "public.xml",
        ] {
            assert!(
                text.contains(&format!("<string>{content_type}</string>")),
                "{content_type} is missing"
            );
        }
        for extension in [
            "java",
            "kt",
            "gradle",
            "md",
            "properties",
            "yml",
            "json",
            "xml",
        ] {
            assert!(
                text.contains(&format!("<string>{extension}</string>")),
                "the .{extension} files are missing"
            );
        }
        let roles = text
            .matches("<key>CFBundleTypeRole</key>\n\t\t\t<string>Editor</string>")
            .count();
        let ranks = text
            .matches("<key>LSHandlerRank</key>\n\t\t\t<string>Alternate</string>")
            .count();
        assert_eq!(
            roles, 11,
            "a type for every kind of document and the folder"
        );
        assert_eq!(ranks, roles, "every type is an alternate handler");
        // The key is for several user sessions of one Mac, not for the windows of one
        // user: Arro runs a process per project.
        assert!(!text.contains("LSMultipleInstancesProhibited"));
        assert_eq!(
            text.matches("<array>").count(),
            text.matches("</array>").count()
        );
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

    #[test]
    fn the_disk_image_is_named_after_the_platform_as_the_zip() {
        assert_eq!(
            dmg_name("0.1.16", "aarch64").unwrap(),
            "arro-0.1.16-macos-aarch64.dmg"
        );
        assert_eq!(
            dmg_name("0.1.16", "x86_64").unwrap(),
            "arro-0.1.16-macos-x86_64.dmg"
        );
        assert!(dmg_name("0.1.16", "riscv64").is_err());
    }

    #[test]
    fn hdiutil_gets_the_volume_the_folder_and_the_format() {
        let command = dmg_command(
            Path::new("/tmp/stage"),
            Path::new("/tmp/arro-0.1.16-macos-aarch64.dmg"),
        );
        assert_eq!(command.get_program(), "hdiutil");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "create",
                "-volname",
                "Arro",
                "-srcfolder",
                "/tmp/stage",
                "-ov",
                "-format",
                "UDZO",
                "/tmp/arro-0.1.16-macos-aarch64.dmg"
            ]
        );
    }
}
