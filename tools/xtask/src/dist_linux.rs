//! The Linux folder and its `tar.gz`: the program, the helper jars, the language tools,
//! the icon, a desktop entry and an optional `install.sh`.
//!
//! The package is built on Linux, where the release binary and the executable bits of
//! `install.sh` come from. Everything that is text is written here, so the layout can be
//! checked from any system.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Result, dist, helpers, notice};

const BINARY: &str = "arro";
const README: &str = include_str!("../../../dist/README-linux.txt");
const DESKTOP: &str = include_str!("../../../dist/arro.desktop");
const INSTALL: &str = include_str!("../../../dist/install.sh");
/// Where the package puts the program and everything next to it.
const OPT_DIR: &str = "/opt/arro";
/// The Debian packages of the libraries the release binary links against. The program
/// is built in Ubuntu 22.04 with `libfontconfig1-dev`, `libwayland-dev`, `libx11-dev`,
/// `libxcb1-dev`, `libxkbcommon-dev`, `libxkbcommon-x11-dev` and Vulkan, so these are
/// the runtime packages of those build packages.
const DEPENDS: [&str; 9] = [
    "libc6",
    "libfontconfig1",
    "libfreetype6",
    "libxkbcommon0",
    "libxkbcommon-x11-0",
    "libwayland-client0",
    "libx11-xcb1",
    "libxcb1",
    "libvulkan1",
];
/// The PNG renders that become the icons of the package, by size.
const ICON_PREFIX: &str = "arro-";
const ICON_SUFFIX: &str = ".png";
/// The files of the release folder that the package puts somewhere other than
/// `/opt/arro`, or does not need: the package manager writes the desktop entry and the
/// icons itself.
const NOT_IN_OPT: [&str; 3] = ["install.sh", "arro.desktop", "arro-256.png"];
/// What `postinst` and `postrm` refresh after the files change, when the tools are
/// installed. A missing tool is not an error.
const MAINTAINER_SCRIPT: &str = "#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
exit 0
";

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn run(root: &Path, out: &Path, tools: Option<&Path>, archive: bool, deb: bool) -> Result {
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
    if deb {
        let package = deb_package(&root, &out)?;
        println!("{}", package.display());
    }
    Ok(())
}

/// The Debian package sits next to the release folder, like the archive. `dpkg-deb`
/// writes it from a tree this module assembles; a system without the tool stops with a
/// message instead of writing half a package.
fn deb_package(root: &Path, out: &Path) -> Result<PathBuf> {
    let parent = out.parent().ok_or("the output folder has no parent")?;
    let archive = parent.join(deb_name(env!("CARGO_PKG_VERSION")));
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let tree = parent.join(format!(".arro-deb-{}-{}", std::process::id(), next_id()));
    let result = build_deb_tree(root, out, &tree).and_then(|()| run_dpkg_deb(&tree, &archive));
    let _ = fs::remove_dir_all(&tree);
    if result.is_err() {
        // A failed run must not leave a package that a later step may attach.
        let _ = fs::remove_file(&archive);
    }
    result?;
    Ok(archive)
}

/// `arro_0.1.16_amd64.deb`: the name the Debian tools expect.
fn deb_name(version: &str) -> String {
    format!("arro_{version}_amd64.deb")
}

/// The control file of the package. `Version` has no `v`, as `dpkg` compares it.
fn control_text(version: &str) -> String {
    format!(
        "Package: arro\n\
         Version: {version}\n\
         Section: devel\n\
         Priority: optional\n\
         Architecture: amd64\n\
         Maintainer: Arro\n\
         Depends: {}\n\
         Description: A native IDE for Java\n\
         \x20Arro is a fast native IDE for Java with its own Rust core.\n",
        DEPENDS.join(", ")
    )
}

/// The desktop entry of the package: the same file the folder carries, with the folder
/// of the program written in.
fn desktop_entry() -> String {
    DESKTOP.replace("@ARRO_DIR@", OPT_DIR)
}

/// Fills `tree` with the files of a `.deb` and leaves `dpkg-deb` to pack it.
fn build_deb_tree(root: &Path, out: &Path, tree: &Path) -> Result {
    let opt = tree.join("opt").join("arro");
    copy_tree(out, &opt, &NOT_IN_OPT)?;

    let bin = tree.join("usr").join("bin");
    create_dir(&bin)?;
    create_symlink(&Path::new(OPT_DIR).join(BINARY), &bin.join(BINARY))?;

    let applications = tree.join("usr").join("share").join("applications");
    create_dir(&applications)?;
    write_file(&applications.join("arro.desktop"), &desktop_entry())?;

    install_icons(root, tree)?;

    let debian = tree.join("DEBIAN");
    create_dir(&debian)?;
    write_file(
        &debian.join("control"),
        &control_text(env!("CARGO_PKG_VERSION")),
    )?;
    write_script(&debian.join("postinst"))?;
    write_script(&debian.join("postrm"))?;
    set_directory_modes(tree)?;
    Ok(())
}

/// A package carries 755 for every folder, whatever the umask of the build was.
fn set_directory_modes(tree: &Path) -> Result {
    set_mode(tree, 0o755)?;
    for entry in fs::read_dir(tree)? {
        let entry = entry?;
        // A link is not a folder and keeps the mode it was created with, so the walk
        // never goes into it or through it.
        if entry.file_type()?.is_dir() {
            set_directory_modes(&entry.path())?;
        }
    }
    Ok(())
}

/// The PNG icons of the Windows build, in the hicolor theme of the system, at every
/// size the build carries a render for.
fn install_icons(root: &Path, tree: &Path) -> Result {
    for (size, source) in icon_sources(root)? {
        let folder = tree
            .join("usr/share/icons/hicolor")
            .join(format!("{size}x{size}/apps"));
        create_dir(&folder)?;
        let target = folder.join("arro.png");
        fs::copy(&source, &target)?;
        set_mode(&target, 0o644)?;
    }
    Ok(())
}

/// `arro-16.png` and `arro-256.png` of the resources, from the smallest. A file of
/// another name, such as the SVG source, stays out of the package.
fn icon_sources(root: &Path) -> Result<Vec<(u32, PathBuf)>> {
    let folder = root.join("crates/app/ide/resources");
    let mut sources = Vec::new();
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(size) = name
            .strip_prefix(ICON_PREFIX)
            .and_then(|rest| rest.strip_suffix(ICON_SUFFIX))
        else {
            continue;
        };
        if let Ok(size) = size.parse() {
            sources.push((size, entry.path()));
        }
    }
    sources.sort();
    if sources.is_empty() {
        return Err(format!("{} has no arro-<size>.png", folder.display()).into());
    }
    Ok(sources)
}

/// `dpkg-deb --build --root-owner-group TREE ARCHIVE`: the owner of every file becomes
/// root, whatever user built the package.
fn deb_command(tree: &Path, archive: &Path) -> Command {
    let mut command = Command::new("dpkg-deb");
    command
        .arg("--build")
        .arg("--root-owner-group")
        .arg(tree)
        .arg(archive);
    command
}

fn run_dpkg_deb(tree: &Path, archive: &Path) -> Result {
    let mut command = deb_command(tree, archive);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("dpkg-deb --build failed: {status}").into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            "dpkg-deb is not available: install dpkg-dev, or build the .deb on Debian \
             or Ubuntu"
                .into(),
        ),
        Err(error) => Err(error.into()),
    }
}

/// Copies the release folder into the package. A file that is executable stays
/// executable, everything else a package carries is readable by all and writable by the
/// owner only.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result {
    create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if skip.contains(&text.as_ref()) {
            continue;
        }
        let target = to.join(&name);
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target, &[])?;
        } else {
            fs::copy(entry.path(), &target)?;
            let mode = if executable(&metadata) { 0o755 } else { 0o644 };
            set_mode(&target, mode)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    // The package is built on Linux, where the bits of the release folder are kept. On
    // another system the folder is only read, so nothing depends on the bits there.
    false
}

fn create_dir(path: &Path) -> Result {
    fs::create_dir_all(path)?;
    set_mode(path, 0o755)
}

fn write_file(path: &Path, text: &str) -> Result {
    dist::write_unix_text(path, text)?;
    set_mode(path, 0o644)
}

fn write_script(path: &Path) -> Result {
    dist::write_unix_text(path, MAINTAINER_SCRIPT)?;
    set_mode(path, 0o755)
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(target: &Path, link: &Path) -> Result {
    // `dpkg-deb` runs on a Debian system; elsewhere a small file keeps the assembled
    // tree checkable. It carries the path a Linux link would, with the Linux separators.
    let text = target.to_string_lossy().replace('\\', "/");
    write_file(link, &format!("{text}\n"))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result {
    use std::os::unix::fs::PermissionsExt;

    // `usr/bin/arro` of the tree points at `/opt/arro/arro`, which does not exist on the
    // build machine. `set_permissions` follows a link target and would fail on the
    // dangling one, and a link that resolved would change a file outside the package.
    // `dpkg` takes the mode of the link itself.
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result {
    // Windows has no such bits, and the package is not built there.
    Ok(())
}

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
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
    use std::path::Path;
    use test_support::TempDir;

    /// `usr/bin/arro` of the tree points at `/opt/arro/arro`, which does not exist on the
    /// build machine: `Path::exists` follows the link and reports no file. The test looks
    /// at the link itself. Where the system has no links, the tree keeps a file with the
    /// target path instead.
    fn link_points_to(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        {
            fs::symlink_metadata(link).is_ok_and(|metadata| metadata.file_type().is_symlink())
                && fs::read_link(link).is_ok_and(|destination| destination == target)
        }
        #[cfg(not(unix))]
        {
            fs::read_to_string(link)
                .is_ok_and(|text| text.trim_end() == target.to_string_lossy().as_ref())
        }
    }

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

    /// The manager of the desktop hands the selected files to the program and groups the
    /// windows of the program under its icon.
    #[test]
    fn the_desktop_entry_opens_the_files_it_is_given() {
        assert!(DESKTOP.contains("Exec=\"@ARRO_DIR@/arro\" %F"));
        assert!(DESKTOP.contains("MimeType="));
        for mime in [
            "inode/directory",
            "text/x-java",
            "text/x-kotlin",
            "text/markdown",
            "application/xml",
            "text/plain",
        ] {
            assert!(DESKTOP.contains(mime), "{mime} is missing");
        }
        assert!(DESKTOP.contains("StartupWMClass=arro"));
        assert!(INSTALL.contains("arro.desktop"));
        assert!(INSTALL.contains("@ARRO_DIR@"));
    }

    #[test]
    fn the_control_file_names_the_package_and_its_libraries() {
        let text = control_text("0.1.16");
        assert!(text.starts_with("Package: arro\n"), "{text}");
        assert!(text.contains("Version: 0.1.16\n"), "{text}");
        assert!(!text.contains("Version: v"), "{text}");
        assert!(text.contains("Architecture: amd64\n"), "{text}");
        assert!(text.contains("Maintainer: Arro\n"), "{text}");
        let depends = text
            .lines()
            .find(|line| line.starts_with("Depends: "))
            .expect("a Depends line");
        for library in DEPENDS {
            assert!(depends.contains(library), "{library} is not in {depends}");
        }
        assert!(text.contains("Description: "), "{text}");
    }

    #[test]
    fn the_package_name_holds_the_version_without_v() {
        assert_eq!(deb_name("0.1.16"), "arro_0.1.16_amd64.deb");
    }

    #[test]
    fn the_package_puts_the_program_in_opt_and_links_it_from_usr_bin() {
        let temp = TempDir::new("dist-deb-tree");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("package");
        let resources = root.join("crates/app/ide/resources");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("arro-16.png"), b"png16").unwrap();
        fs::write(resources.join("arro-256.png"), b"png256").unwrap();
        fs::create_dir_all(out.join("helpers")).unwrap();
        fs::create_dir_all(out.join("tools/engine")).unwrap();
        fs::write(out.join("arro"), b"program").unwrap();
        dist::make_executable(&out.join("arro")).unwrap();
        fs::write(out.join("helpers/one.jar"), b"jar").unwrap();
        fs::write(out.join("tools/engine/server.jar"), b"server").unwrap();
        fs::write(out.join("README.txt"), b"readme").unwrap();
        fs::write(out.join("THIRD-PARTY.txt"), b"licenses").unwrap();
        fs::write(out.join("NOTICE.txt"), b"notice").unwrap();
        fs::write(out.join("install.sh"), b"installer").unwrap();
        fs::write(out.join("arro.desktop"), b"entry").unwrap();
        fs::write(out.join("arro-256.png"), b"png256").unwrap();

        build_deb_tree(&root, &out, &tree).unwrap();

        let opt = tree.join("opt/arro");
        assert_eq!(fs::read(opt.join("arro")).unwrap(), b"program");
        assert!(opt.join("helpers/one.jar").is_file());
        assert!(opt.join("tools/engine/server.jar").is_file());
        assert!(opt.join("README.txt").is_file());
        assert!(opt.join("NOTICE.txt").is_file());
        // The package manager writes these itself.
        assert!(!opt.join("install.sh").exists());
        assert!(!opt.join("arro.desktop").exists());
        assert!(!opt.join("arro-256.png").exists());

        assert!(link_points_to(
            &tree.join("usr/bin/arro"),
            Path::new("/opt/arro/arro")
        ));
        let entry = fs::read_to_string(tree.join("usr/share/applications/arro.desktop")).unwrap();
        assert!(entry.contains("/opt/arro/arro"), "{entry}");
        assert!(entry.contains("%F"), "{entry}");
        assert!(!entry.contains("@ARRO_DIR@"), "{entry}");
        assert!(
            tree.join("usr/share/icons/hicolor/16x16/apps/arro.png")
                .is_file()
        );
        assert!(
            tree.join("usr/share/icons/hicolor/256x256/apps/arro.png")
                .is_file()
        );
        assert!(tree.join("DEBIAN/control").is_file());
        assert!(tree.join("DEBIAN/postinst").is_file());
        assert!(tree.join("DEBIAN/postrm").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn the_files_of_the_package_have_the_mode_dpkg_expects() {
        use std::os::unix::fs::PermissionsExt;

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        let temp = TempDir::new("dist-deb-modes");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("package");
        let resources = root.join("crates/app/ide/resources");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("arro-16.png"), b"png16").unwrap();
        fs::write(resources.join("arro-256.png"), b"png256").unwrap();
        fs::create_dir_all(out.join("helpers")).unwrap();
        fs::write(out.join("arro"), b"program").unwrap();
        dist::make_executable(&out.join("arro")).unwrap();
        fs::write(out.join("helpers/one.jar"), b"jar").unwrap();
        fs::write(out.join("install.sh"), b"installer").unwrap();

        build_deb_tree(&root, &out, &tree).unwrap();

        assert_eq!(mode(&tree.join("opt/arro/arro")), 0o755);
        assert_eq!(mode(&tree.join("opt/arro/helpers/one.jar")), 0o644);
        assert_eq!(mode(&tree.join("opt/arro")), 0o755);
        assert_eq!(mode(&tree.join("DEBIAN/control")), 0o644);
        assert_eq!(mode(&tree.join("DEBIAN/postinst")), 0o755);
        assert!(link_points_to(
            &tree.join("usr/bin/arro"),
            Path::new("/opt/arro/arro")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn setting_the_mode_of_a_link_leaves_the_link_alone() {
        let temp = TempDir::new("dist-deb-link-mode");
        let link = temp.path().join("arro");
        create_symlink(Path::new("/opt/arro/arro"), &link).unwrap();

        // The target does not exist here, so a `chmod` that follows the link fails.
        set_mode(&link, 0o644).unwrap();
        assert!(link_points_to(&link, Path::new("/opt/arro/arro")));
    }

    #[test]
    fn dpkg_deb_gets_the_tree_and_the_archive_root_owned() {
        let tree = Path::new("/tmp/tree");
        let archive = Path::new("/tmp/arro_0.1.16_amd64.deb");
        let command = deb_command(tree, archive);
        assert_eq!(command.get_program(), "dpkg-deb");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--build",
                "--root-owner-group",
                "/tmp/tree",
                "/tmp/arro_0.1.16_amd64.deb"
            ]
        );
    }

    /// A real package needs `dpkg-deb` from `dpkg-dev`, which a Debian or Ubuntu system
    /// has. The test reports a skip elsewhere instead of failing.
    #[test]
    #[ignore = "needs dpkg-deb; run with --ignored on Debian or Ubuntu"]
    fn dpkg_deb_builds_a_real_package() {
        let available = Command::new("dpkg-deb").arg("--version").output();
        let Ok(available) = available else {
            eprintln!("dpkg-deb is not installed; the test is skipped");
            return;
        };
        assert!(available.status.success());

        let temp = TempDir::new("dist-deb-real");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("package");
        let resources = root.join("crates/app/ide/resources");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("arro-16.png"), b"png16").unwrap();
        fs::write(resources.join("arro-256.png"), b"png256").unwrap();
        fs::create_dir_all(out.join("tools")).unwrap();
        fs::write(out.join("arro"), b"program").unwrap();
        dist::make_executable(&out.join("arro")).unwrap();
        fs::write(out.join("tools/server.jar"), b"jar").unwrap();

        build_deb_tree(&root, &out, &tree).unwrap();
        let archive = temp.path().join(deb_name(env!("CARGO_PKG_VERSION")));
        let status = deb_command(&tree, &archive).status().unwrap();
        assert!(status.success());

        let info = Command::new("dpkg-deb")
            .args(["--info", archive.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(info.status.success());
        let info = String::from_utf8_lossy(&info.stdout);
        assert!(info.contains("Package: arro"), "{info}");
        assert!(
            info.contains(env!("CARGO_PKG_VERSION")),
            "the version is not in {info}"
        );

        let contents = Command::new("dpkg-deb")
            .args(["--contents", archive.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(contents.status.success());
        let contents = String::from_utf8_lossy(&contents.stdout);
        for path in [
            "./opt/arro/arro",
            "./usr/bin/arro",
            "./usr/share/applications/arro.desktop",
            "./usr/share/icons/hicolor/16x16/apps/arro.png",
        ] {
            assert!(contents.contains(path), "{path} is not in {contents}");
        }
    }
}
