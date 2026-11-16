//! The Linux folder, its `tar.gz` and the packages built from it: the `.deb`, the `.rpm`
//! and the AppImage carry the program, the helper jars, the language tools, the icon, a
//! desktop entry and an `install.sh`. All three are packed from one tree.
//!
//! The folder and the packages are built on Linux, where the release binary and the
//! executable bits of `install.sh` come from. Everything that is text is written here, so
//! the layout can be checked from any system.

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
/// The license of the project, as `LICENSE.md` and the README name it. The packages
/// carry the same string.
const LICENSE: &str = "FSL-1.1-ALv2";
/// The release number of a package: the first build of a version.
const RELEASE: &str = "1";
/// The architecture of the packages. Only x86-64 is built.
const ARCH: &str = "x86_64";
/// The libraries the release binary links against, in the names of Debian and of Fedora
/// and openSUSE. The program is built in Ubuntu 22.04 with `libfontconfig1-dev`,
/// `libwayland-dev`, `libx11-dev`, `libxcb1-dev`, `libxkbcommon-dev`,
/// `libxkbcommon-x11-dev` and Vulkan, so these are the runtime packages of those build
/// packages. The second name is the one `dnf` and `zypper` know.
const LIBRARIES: [(&str, &str); 9] = [
    ("libc6", "glibc"),
    ("libfontconfig1", "fontconfig"),
    ("libfreetype6", "freetype"),
    ("libxkbcommon0", "libxkbcommon"),
    ("libxkbcommon-x11-0", "libxkbcommon-x11"),
    ("libwayland-client0", "libwayland-client"),
    ("libx11-xcb1", "libX11-xcb"),
    ("libxcb1", "libxcb"),
    ("libvulkan1", "vulkan-loader"),
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
/// The tools that make the package, with the command that installs them on the systems
/// of the release.
const RPMBUILD_MISSING: &str = "rpmbuild is not available: install the rpm-build \
                                 package (dnf install rpm-build, or zypper install \
                                 rpm-build), or build the .rpm on Fedora or openSUSE";
/// The variable that names the program packing an AppDir, for a machine where it is not
/// in the PATH.
const APPIMAGETOOL_ENV: &str = "APPIMAGETOOL";
/// Where the release lives inside the AppDir.
const APPDIR_RELEASE: &str = "usr";
/// The launcher of the image. The runtime names the folder of the image in `APPDIR`; a
/// folder unpacked by hand finds it next to this script.
const APPRUN: &str = "#!/bin/sh
set -e
here=${APPDIR:-$(dirname \"$0\")}
exec \"$here/usr/arro\" \"$@\"
";
/// The tool that packs the AppDir. Nothing downloads it: the message names the variable
/// and the project that builds the tool.
const APPIMAGETOOL_MISSING: &str = "appimagetool is not available: set APPIMAGETOOL to the \
                                   program, or install it from \
                                   https://github.com/AppImage/appimagetool/releases";

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn run(
    root: &Path,
    out: &Path,
    tools: Option<&Path>,
    archive: bool,
    deb: bool,
    rpm: bool,
    appimage: bool,
) -> Result {
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
    if rpm {
        let package = rpm_package(&root, &out)?;
        println!("{}", package.display());
    }
    if appimage {
        let image = appimage_file(&root, &out)?;
        println!("{}", image.display());
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
    let depends = LIBRARIES
        .iter()
        .map(|(deb, _)| *deb)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Package: arro\n\
         Version: {version}\n\
         Section: devel\n\
         Priority: optional\n\
         Architecture: amd64\n\
         Maintainer: Arro\n\
         License: {LICENSE}\n\
         Depends: {depends}\n\
         Description: A native IDE for Java\n\
         \x20Arro is a fast native IDE for Java with its own Rust core.\n"
    )
}

/// The desktop entry of the package: the same file the folder carries, with the folder
/// of the program written in.
fn desktop_entry() -> String {
    desktop_entry_for(&format!("\"{OPT_DIR}/{BINARY}\""))
}

/// The desktop entry for a program named by `program`. The packages write the folder of
/// the program; an AppImage names its launcher, which the runtime starts from the root
/// of the unpacked image. The associations are the same in both, so a file manager and
/// the system keep one entry for the program.
fn desktop_entry_for(program: &str) -> String {
    DESKTOP.replace("\"@ARRO_DIR@/arro\"", program)
}

/// Fills `tree` with the files both packages carry: the release folder under
/// `/opt/arro`, the link in `/usr/bin`, the desktop entry and the icons of the system.
fn build_package_tree(root: &Path, out: &Path, tree: &Path) -> Result {
    let opt = tree.join("opt").join("arro");
    copy_tree(out, &opt, &NOT_IN_OPT)?;

    let bin = tree.join("usr").join("bin");
    create_dir(&bin)?;
    create_symlink(&Path::new(OPT_DIR).join(BINARY), &bin.join(BINARY))?;

    let applications = tree.join("usr").join("share").join("applications");
    create_dir(&applications)?;
    write_file(&applications.join("arro.desktop"), &desktop_entry())?;

    install_icons(root, tree)?;
    set_directory_modes(tree)?;
    Ok(())
}

/// Fills `tree` with the files of a `.deb` and leaves `dpkg-deb` to pack it.
fn build_deb_tree(root: &Path, out: &Path, tree: &Path) -> Result {
    build_package_tree(root, out, tree)?;

    let debian = tree.join("DEBIAN");
    create_dir(&debian)?;
    write_file(
        &debian.join("control"),
        &control_text(env!("CARGO_PKG_VERSION")),
    )?;
    write_script(&debian.join("postinst"))?;
    write_script(&debian.join("postrm"))?;
    set_directory_modes(&debian)?;
    Ok(())
}

/// The `.rpm` sits next to the release folder too. `rpmbuild` packs the same tree the
/// `.deb` is built from, through a spec this module writes; a system without the tool
/// stops with a message instead of writing half a package.
fn rpm_package(root: &Path, out: &Path) -> Result<PathBuf> {
    let parent = out.parent().ok_or("the output folder has no parent")?;
    let archive = parent.join(rpm_name(env!("CARGO_PKG_VERSION")));
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let work = parent.join(format!(".arro-rpm-{}-{}", std::process::id(), next_id()));
    let result = build_rpm_work(root, out, &work).and_then(|built| copy_rpm(&built, &archive));
    let _ = fs::remove_dir_all(&work);
    if result.is_err() {
        // A failed run must not leave a package that a later step may attach.
        let _ = fs::remove_file(&archive);
    }
    result?;
    Ok(archive)
}

/// `arro-0.1.16-1.x86_64.rpm`: the name `rpmbuild` gives the package of the spec.
fn rpm_name(version: &str) -> String {
    format!("arro-{version}-{RELEASE}.{ARCH}.rpm")
}

/// Assembles the tree in `work/rpmbuild/BUILD/stage`, writes the spec and calls
/// `rpmbuild -bb`.
fn build_rpm_work(root: &Path, out: &Path, work: &Path) -> Result<PathBuf> {
    // The release folder holds the language servers: a system without the tool should
    // say so before the tree is copied, not after minutes of work.
    match Command::new("rpmbuild").arg("--version").output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => return Err(format!("rpmbuild --version failed: {}", output.status).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RPMBUILD_MISSING.into());
        }
        Err(error) => return Err(error.into()),
    }
    let top = work.join("rpmbuild");
    for folder in ["BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"] {
        fs::create_dir_all(top.join(folder))?;
    }
    let stage = top.join("BUILD").join("stage");
    build_package_tree(root, out, &stage)?;
    let spec = top.join("SPECS").join("arro.spec");
    write_file(&spec, &spec_text(env!("CARGO_PKG_VERSION"), &stage)?)?;

    let mut command = rpmbuild_command(&top, &spec, &stage);
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => return Err(format!("rpmbuild -bb failed: {status}").into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RPMBUILD_MISSING.into());
        }
        Err(error) => return Err(error.into()),
    }
    let built = top
        .join("RPMS")
        .join(ARCH)
        .join(rpm_name(env!("CARGO_PKG_VERSION")));
    if !built.is_file() {
        return Err(format!("rpmbuild wrote no {}", built.display()).into());
    }
    Ok(built)
}

/// Puts the package `rpmbuild` wrote next to the release folder under the name the
/// release publishes.
fn copy_rpm(built: &Path, archive: &Path) -> Result {
    if archive.exists() {
        fs::remove_file(archive)?;
    }
    fs::copy(built, archive)?;
    Ok(())
}

/// `rpmbuild -bb --define "_topdir TOP" --define "arro_stage STAGE" SPEC`. The tree the
/// spec installs is passed as a macro: from rpm 4.20 on `%{_builddir}` is a folder of the
/// package itself, so `%install` cannot find the tree through it.
fn rpmbuild_command(top: &Path, spec: &Path, stage: &Path) -> Command {
    let mut command = Command::new("rpmbuild");
    command
        .arg("-bb")
        .arg("--define")
        .arg(format!("_topdir {}", top.display()))
        .arg("--define")
        .arg(format!("arro_stage {}", stage.display()))
        .arg(spec);
    command
}

/// The `.AppImage` of the release sits next to the folder like the packages: one file
/// that runs on a system without `dpkg` and `rpm`. `appimagetool` packs an AppDir this
/// module assembles; a system without the tool stops with a message instead of writing
/// half an image.
fn appimage_file(root: &Path, out: &Path) -> Result<PathBuf> {
    let parent = out.parent().ok_or("the output folder has no parent")?;
    let image = parent.join(appimage_name(env!("CARGO_PKG_VERSION")));
    if image.exists() {
        fs::remove_file(&image)?;
    }
    // A system without the tool should say so before the tree is copied, not after
    // minutes of work.
    check_appimagetool()?;
    let tree = parent.join(format!(
        ".arro-appimage-{}-{}",
        std::process::id(),
        next_id()
    ));
    let result = build_appdir(root, out, &tree).and_then(|()| run_appimagetool(&tree, &image));
    let _ = fs::remove_dir_all(&tree);
    if result.is_err() {
        // A failed run must not leave an image that a later step may attach.
        let _ = fs::remove_file(&image);
    }
    result?;
    Ok(image)
}

/// `arro-0.1.16-x86_64.AppImage`: the name the tool writes and the updater looks for.
fn appimage_name(version: &str) -> String {
    format!("arro-{version}-{ARCH}.AppImage")
}

/// Fills `app_dir` from the release tree the `.deb` and the `.rpm` are built from and
/// adds what the AppImage specification wants at the root: the launcher, the desktop
/// entry, the 256 render of the icon and `.DirIcon`. The program and everything next to
/// it live under `usr/`.
fn build_appdir(root: &Path, out: &Path, app_dir: &Path) -> Result {
    copy_tree(out, &app_dir.join(APPDIR_RELEASE), &NOT_IN_OPT)?;

    let app_run = app_dir.join("AppRun");
    dist::write_unix_text(&app_run, APPRUN)?;
    set_mode(&app_run, 0o755)?;
    write_file(&app_dir.join("arro.desktop"), &appimage_desktop_entry())?;

    let (_, icon) = icon_sources(root)?
        .into_iter()
        .find(|(size, _)| *size == 256)
        .ok_or("the resources have no 256x256 render of the icon")?;
    for name in ["arro.png", ".DirIcon"] {
        let target = app_dir.join(name);
        fs::copy(&icon, &target)?;
        set_mode(&target, 0o644)?;
    }
    set_directory_modes(app_dir)?;
    Ok(())
}

/// The desktop entry of the image: the same associations as the packages, with the
/// launcher of the AppDir as the program. The entry stays inside the image, where no
/// program of the system is on the PATH: a `TryExec` that names the launcher would hide
/// the entry after a desktop integration, and the integration writes `Exec` itself.
fn appimage_desktop_entry() -> String {
    desktop_entry_for("AppRun")
        .replace("\r\n", "\n")
        .replace("TryExec=AppRun\n", "")
}

/// The program that packs the AppDir: `APPIMAGETOOL` when it names one, `appimagetool`
/// from the PATH otherwise. Nothing is downloaded here.
fn appimagetool_program() -> PathBuf {
    std::env::var_os(APPIMAGETOOL_ENV)
        .filter(|program| !program.is_empty())
        .map_or_else(|| PathBuf::from("appimagetool"), PathBuf::from)
}

/// The command of the tool with the environment the packing needs: the tool is an AppImage
/// in most downloads, and a runner without FUSE unpacks it itself when the runtime reads
/// `APPIMAGE_EXTRACT_AND_RUN`. The architecture is the one the packages are built for.
fn appimagetool_base() -> Command {
    let mut command = Command::new(appimagetool_program());
    command
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .env("ARCH", ARCH);
    command
}

fn check_appimagetool() -> Result {
    let mut command = appimagetool_base();
    command.arg("--version");
    match command.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!("appimagetool --version failed: {}", output.status).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(APPIMAGETOOL_MISSING.into())
        }
        Err(error) => Err(error.into()),
    }
}

/// `appimagetool APPDIR IMAGE`.
fn appimagetool_command(app_dir: &Path, image: &Path) -> Command {
    let mut command = appimagetool_base();
    command.arg(app_dir).arg(image);
    command
}

fn run_appimagetool(app_dir: &Path, image: &Path) -> Result {
    let mut command = appimagetool_command(app_dir, image);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("appimagetool failed: {status}").into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(APPIMAGETOOL_MISSING.into())
        }
        Err(error) => Err(error.into()),
    }
}

/// A file or folder of the package tree, as `%files` names it.
struct PackageEntry {
    /// The path in the package, with `/` separators and a leading `/`.
    path: String,
    /// The mode of a file; `None` for a folder and a symbolic link.
    mode: Option<u32>,
    folder: bool,
}

/// The spec of the package: no sources are built, `%install` copies the assembled tree
/// into the build root and `%files` names every path of that tree with its mode.
fn spec_text(version: &str, tree: &Path) -> Result<String> {
    let mut text = String::new();
    // Nothing is built here, and the tree holds complete JDK and language server folders
    // that the post-install steps would strip and rewrite: the debug package would be
    // empty too. All three have to go before any section is read.
    text.push_str("%global debug_package %{nil}\n");
    text.push_str("%global __os_install_post %{nil}\n");
    text.push_str("%global __brp_mangle_shebangs %{nil}\n");
    text.push_str(&format!(
        "\nName: arro\nVersion: {version}\nRelease: {RELEASE}\n"
    ));
    text.push_str("Summary: A native IDE for Java\n");
    text.push_str(&format!("License: {LICENSE}\n"));
    text.push_str("URL: https://github.com/PabloSensei/arro\n");
    text.push_str(&format!("BuildArch: {ARCH}\n"));
    // The binary links against the libraries below and nothing is built here, so the
    // automatic dependency generator has nothing to read and must stay out.
    text.push_str("AutoReqProv: no\n");
    for (_, rpm) in LIBRARIES {
        text.push_str(&format!("Requires: {rpm}\n"));
    }
    text.push_str("\n%description\n");
    text.push_str("Arro is a fast native IDE for Java with its own Rust core.\n");
    text.push_str("\n%install\n");
    text.push_str("rm -rf %{buildroot}\n");
    text.push_str("mkdir -p %{buildroot}\n");
    // The path comes from the command line, so it is quoted here: a build folder may
    // hold spaces.
    text.push_str("cp -a \"%{arro_stage}/.\" %{buildroot}/\n");
    text.push_str("\n%files\n");
    let inside_opt = format!("{OPT_DIR}/");
    for entry in package_entries(tree)? {
        if entry.folder {
            // Only the folders of the program belong to this package. `/opt`, `/usr`,
            // `/usr/bin` and the folders of the icon theme are owned by the packages of
            // the system, and naming them here fails the install with a file conflict;
            // the files inside them are still below.
            if entry.path == OPT_DIR || entry.path.starts_with(&inside_opt) {
                text.push_str(&format!("%attr(0755,root,root) %dir {}\n", entry.path));
            }
        } else if let Some(mode) = entry.mode {
            text.push_str(&format!("%attr({mode:04o},root,root) {}\n", entry.path));
        } else {
            // A link has no mode of its own; 0777 is what a package records for it.
            text.push_str(&format!("%attr(0777,root,root) {}\n", entry.path));
        }
    }
    Ok(text)
}

fn package_entries(tree: &Path) -> Result<Vec<PackageEntry>> {
    let mut entries = Vec::new();
    collect_entries(tree, tree, &mut entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn collect_entries(root: &Path, directory: &Path, entries: &mut Vec<PackageEntry>) -> Result {
    let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            entries.push(PackageEntry {
                path: format!("/{relative}"),
                mode: None,
                folder: false,
            });
        } else if metadata.is_dir() {
            entries.push(PackageEntry {
                path: format!("/{relative}"),
                mode: None,
                folder: true,
            });
            collect_entries(root, &path, entries)?;
        } else {
            let mode = if executable(&metadata) { 0o755 } else { 0o644 };
            entries.push(PackageEntry {
                path: format!("/{relative}"),
                mode: Some(mode),
                folder: false,
            });
        }
    }
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
    #[cfg(unix)]
    use std::ffi::OsStr;
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
        for (library, _) in LIBRARIES {
            assert!(depends.contains(library), "{library} is not in {depends}");
        }
        assert!(text.contains(&format!("License: {LICENSE}\n")), "{text}");
        assert!(text.contains("Description: "), "{text}");
    }

    #[test]
    fn the_package_name_holds_the_version_without_v() {
        assert_eq!(deb_name("0.1.16"), "arro_0.1.16_amd64.deb");
    }

    #[test]
    fn the_rpm_name_holds_the_version_the_release_and_the_architecture() {
        assert_eq!(rpm_name("0.1.16"), "arro-0.1.16-1.x86_64.rpm");
    }

    /// The release folder of the tests: the program, one helper jar, one tool jar, the
    /// texts, the desktop entry and the two renders of the icon.
    fn release_tree(root: &Path, out: &Path) {
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
        fs::write(out.join("NOTICE.txt"), b"notice").unwrap();
        dist::write_unix_text(&out.join("install.sh"), INSTALL).unwrap();
        dist::make_executable(&out.join("install.sh")).unwrap();
        fs::write(out.join("arro.desktop"), DESKTOP).unwrap();
        fs::write(out.join("arro-256.png"), b"png256").unwrap();
    }

    #[test]
    fn the_appimage_name_holds_the_version_and_the_architecture() {
        assert_eq!(appimage_name("0.1.16"), "arro-0.1.16-x86_64.AppImage");
    }

    /// The launcher finds the folder of the release: the runtime names it in `APPDIR`,
    /// and a folder unpacked by hand leaves the variable out.
    #[test]
    fn the_launcher_reads_the_folder_and_starts_the_program() {
        assert!(APPRUN.starts_with("#!/bin/sh\n"), "{APPRUN}");
        assert!(
            APPRUN.contains("here=${APPDIR:-$(dirname \"$0\")}"),
            "{APPRUN}"
        );
        assert!(
            APPRUN.contains("exec \"$here/usr/arro\" \"$@\""),
            "{APPRUN}"
        );
    }

    /// The AppDir carries the release under `usr/` and the files the image needs at the
    /// root: the launcher, the desktop entry, the 256 render and `.DirIcon`.
    #[test]
    fn the_appdir_holds_the_release_the_launcher_and_the_desktop_entry() {
        let temp = TempDir::new("dist-appimage-tree");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let app_dir = temp.path().join("AppDir");
        release_tree(&root, &out);

        build_appdir(&root, &out, &app_dir).unwrap();

        assert_eq!(fs::read(app_dir.join("usr/arro")).unwrap(), b"program");
        assert!(app_dir.join("usr/helpers/one.jar").is_file());
        assert!(app_dir.join("usr/tools/engine/server.jar").is_file());
        assert!(app_dir.join("usr/README.txt").is_file());
        assert!(app_dir.join("usr/NOTICE.txt").is_file());
        // The root holds what the runtime and the desktop look for; a copy inside
        // `usr/` would be a second, stale one.
        assert!(!app_dir.join("usr/install.sh").exists());
        assert!(!app_dir.join("usr/arro.desktop").exists());
        assert!(!app_dir.join("usr/arro-256.png").exists());

        let entry = fs::read_to_string(app_dir.join("arro.desktop")).unwrap();
        assert!(entry.contains("Exec=AppRun %F"), "{entry}");
        assert!(!entry.contains("TryExec="), "{entry}");
        assert!(entry.contains("Icon=arro"), "{entry}");
        assert!(!entry.contains("@ARRO_DIR@"), "{entry}");
        // A file manager sees the same association as with a package.
        let mime = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("MimeType="))
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(mime(&entry), mime(&desktop_entry()));
        assert!(entry.contains("StartupWMClass=arro"), "{entry}");

        assert_eq!(fs::read(app_dir.join("arro.png")).unwrap(), b"png256");
        assert_eq!(fs::read(app_dir.join(".DirIcon")).unwrap(), b"png256");
        assert_eq!(fs::read_to_string(app_dir.join("AppRun")).unwrap(), APPRUN);
    }

    #[cfg(unix)]
    #[test]
    fn the_launcher_of_the_appdir_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        let temp = TempDir::new("dist-appimage-modes");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let app_dir = temp.path().join("AppDir");
        release_tree(&root, &out);

        build_appdir(&root, &out, &app_dir).unwrap();

        assert_eq!(mode(&app_dir.join("AppRun")), 0o755);
        assert_eq!(mode(&app_dir.join("usr/arro")), 0o755);
        assert_eq!(mode(&app_dir.join("arro.desktop")), 0o644);
        assert_eq!(mode(&app_dir.join("arro.png")), 0o644);
        assert_eq!(mode(&app_dir), 0o755);
    }

    #[test]
    fn appimagetool_gets_the_appdir_the_image_and_the_architecture() {
        let app_dir = Path::new("/tmp/AppDir");
        let image = Path::new("/tmp/arro-0.1.16-x86_64.AppImage");
        let command = appimagetool_command(app_dir, image);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["/tmp/AppDir", "/tmp/arro-0.1.16-x86_64.AppImage"]);
        let env: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        // The tool is an AppImage itself in most downloads, and the runner may have no
        // FUSE.
        assert!(
            env.contains(&("APPIMAGE_EXTRACT_AND_RUN".to_owned(), Some("1".to_owned()))),
            "{env:?}"
        );
        assert!(
            env.contains(&("ARCH".to_owned(), Some(ARCH.to_owned()))),
            "{env:?}"
        );
    }

    /// A real image needs `appimagetool`, which is not part of the repository. The test
    /// reports a skip where the tool is missing instead of failing.
    #[test]
    fn appimagetool_builds_a_real_image() {
        if check_appimagetool().is_err() {
            eprintln!("appimagetool is not installed; the test is skipped");
            return;
        }

        let temp = TempDir::new("dist-appimage-real");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        release_tree(&root, &out);

        let image = appimage_file(&root, &out).unwrap();
        assert_eq!(
            image.file_name().unwrap().to_string_lossy().into_owned(),
            appimage_name(env!("CARGO_PKG_VERSION"))
        );

        // The image carries the AppDir: the runtime extracts it without FUSE.
        let extracted = temp.path().join("extracted");
        fs::create_dir_all(&extracted).unwrap();
        let status = Command::new(&image)
            .arg("--appimage-extract")
            .current_dir(&extracted)
            .status()
            .unwrap();
        assert!(status.success());
        let unpacked = extracted.join("squashfs-root");
        for path in [
            "AppRun",
            "arro.desktop",
            "arro.png",
            "usr/arro",
            "usr/helpers/one.jar",
        ] {
            assert!(unpacked.join(path).exists(), "{path} is not in the image");
        }
        assert_eq!(fs::read_to_string(unpacked.join("AppRun")).unwrap(), APPRUN);
    }

    /// The spec builds no sources: `%install` copies the tree the `.deb` is assembled
    /// from and `%files` names every path of it with the mode it has there.
    #[test]
    fn the_spec_names_the_package_the_version_and_the_fedora_libraries() {
        let temp = TempDir::new("dist-rpm-spec");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("stage");
        release_tree(&root, &out);
        build_package_tree(&root, &out, &tree).unwrap();

        let text = spec_text("0.1.16", &tree).unwrap();
        assert!(text.contains("\nName: arro\n"), "{text}");
        assert!(text.contains("\nVersion: 0.1.16\n"), "{text}");
        assert!(text.contains("\nRelease: 1\n"), "{text}");
        assert!(text.contains(&format!("BuildArch: {ARCH}\n")), "{text}");
        assert!(text.contains(&format!("License: {LICENSE}\n")), "{text}");
        assert!(text.contains("AutoReqProv: no\n"), "{text}");
        for (_, rpm) in LIBRARIES {
            assert!(text.contains(&format!("Requires: {rpm}\n")), "{text}");
        }
        assert!(
            !text.contains("\n%build\n"),
            "nothing is built here: {text}"
        );
        assert!(text.contains("\n%install\n"), "{text}");
        assert!(
            text.contains("cp -a \"%{arro_stage}/.\" %{buildroot}/\n"),
            "{text}"
        );
        assert!(!text.contains("%{_builddir}"), "{text}");
        // The `%files` list of the tree the `.deb` is built from too.
        assert!(text.contains("%dir /opt/arro\n"), "{text}");
        assert!(text.contains("%dir /opt/arro/helpers\n"), "{text}");
        assert!(
            text.contains("%attr(0644,root,root) /opt/arro/helpers/one.jar\n"),
            "{text}"
        );
        assert!(
            text.contains("%attr(0644,root,root) /opt/arro/tools/engine/server.jar\n"),
            "{text}"
        );
        assert!(text.contains("/usr/bin/arro\n"), "{text}");
        assert!(
            text.contains("%attr(0644,root,root) /usr/share/applications/arro.desktop\n"),
            "{text}"
        );
        assert!(
            text.contains("/usr/share/icons/hicolor/16x16/apps/arro.png\n"),
            "{text}"
        );
        assert!(
            text.contains("/usr/share/icons/hicolor/256x256/apps/arro.png\n"),
            "{text}"
        );
        assert!(!text.contains("@ARRO_DIR@"), "{text}");
        // The package manager writes these itself.
        assert!(!text.contains("/opt/arro/install.sh\n"), "{text}");
        assert!(!text.contains("/opt/arro/arro.desktop\n"), "{text}");
        // The link into `/usr/bin` is a link, not a file with a mode of its own.
        #[cfg(unix)]
        {
            assert!(
                text.contains("%attr(0777,root,root) /usr/bin/arro\n"),
                "{text}"
            );
            assert!(
                text.contains("%attr(0755,root,root) /opt/arro/arro\n"),
                "{text}"
            );
        }
    }

    /// `/opt`, `/usr`, `/usr/bin` and the folders of the icon theme belong to the
    /// packages of the system; a package that declares them conflicts with those. Only
    /// the folders of the program are directories of this package.
    #[test]
    fn the_spec_declares_only_the_folders_of_the_program() {
        let temp = TempDir::new("dist-rpm-dirs");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("stage");
        release_tree(&root, &out);
        build_package_tree(&root, &out, &tree).unwrap();

        let text = spec_text("0.1.16", &tree).unwrap();
        let dirs: Vec<&str> = text
            .lines()
            .filter_map(|line| line.split_once("%dir ").map(|(_, dir)| dir))
            .collect();
        assert!(dirs.contains(&"/opt/arro"), "{dirs:?}");
        assert!(dirs.contains(&"/opt/arro/helpers"), "{dirs:?}");
        for dir in &dirs {
            assert!(
                *dir == OPT_DIR || dir.starts_with("/opt/arro/"),
                "{dir} is not a folder of the program"
            );
        }
        assert!(!dirs.contains(&"/opt"), "{dirs:?}");
        assert!(!dirs.contains(&"/usr"), "{dirs:?}");
        assert!(!dirs.contains(&"/usr/bin"), "{dirs:?}");
        assert!(!dirs.contains(&"/usr/share"), "{dirs:?}");
        assert!(!dirs.contains(&"/usr/share/applications"), "{dirs:?}");
    }

    /// `rpmbuild` would run its debug package and its post-install scripts even without
    /// sources: the first fails on an empty list, the others strip and rewrite the JDK
    /// and the language servers of the tree.
    #[test]
    fn the_spec_turns_the_post_processing_of_rpmbuild_off() {
        let temp = TempDir::new("dist-rpm-post");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        let tree = temp.path().join("stage");
        release_tree(&root, &out);
        build_package_tree(&root, &out, &tree).unwrap();

        let text = spec_text("0.1.16", &tree).unwrap();
        let head = text.lines().take(3).collect::<Vec<_>>();
        assert_eq!(
            head,
            [
                "%global debug_package %{nil}",
                "%global __os_install_post %{nil}",
                "%global __brp_mangle_shebangs %{nil}",
            ],
            "{text}"
        );
        // The definitions come before every section that reads them.
        let name = text.find("\nName: arro\n").unwrap();
        for macro_line in [
            "%global debug_package %{nil}",
            "%global __os_install_post %{nil}",
            "%global __brp_mangle_shebangs %{nil}",
        ] {
            let at = text.find(macro_line).unwrap();
            assert!(at < name, "{macro_line} comes after Name: {text}");
        }
    }

    #[test]
    fn rpmbuild_gets_the_top_folder_the_stage_and_the_spec() {
        let top = Path::new("/tmp/rpmbuild");
        let stage = Path::new("/tmp/rpmbuild/BUILD/stage");
        let spec = Path::new("/tmp/rpmbuild/SPECS/arro.spec");
        let command = rpmbuild_command(top, spec, stage);
        assert_eq!(command.get_program(), "rpmbuild");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-bb",
                "--define",
                "_topdir /tmp/rpmbuild",
                "--define",
                "arro_stage /tmp/rpmbuild/BUILD/stage",
                "/tmp/rpmbuild/SPECS/arro.spec"
            ]
        );
    }

    #[test]
    fn rpmbuild_gets_a_stage_path_with_spaces() {
        let top = Path::new("/tmp/rpmbuild");
        let stage = Path::new("/tmp/arro build/stage");
        let spec = Path::new("/tmp/rpmbuild/SPECS/arro.spec");
        let command = rpmbuild_command(top, spec, stage);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        // The path stays one argument of `--define`, and `%install` quotes the macro.
        assert_eq!(args[4], "arro_stage /tmp/arro build/stage", "{args:?}");
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

    /// A real `.rpm` needs `rpmbuild` from the `rpm-build` package and `rpm` itself. The
    /// test reports a skip where they are missing instead of failing.
    #[test]
    fn rpmbuild_builds_a_real_package() {
        for tool in ["rpmbuild", "rpm"] {
            let output = Command::new(tool).arg("--version").output();
            let Ok(output) = output else {
                eprintln!("{tool} is not installed; the test is skipped");
                return;
            };
            if !output.status.success() {
                eprintln!("{tool} does not run; the test is skipped");
                return;
            }
        }

        let temp = TempDir::new("dist-rpm-real");
        let root = temp.path().join("repo");
        let out = temp.path().join("dist");
        release_tree(&root, &out);

        let archive = rpm_package(&root, &out).unwrap();
        assert_eq!(
            archive.file_name().unwrap().to_string_lossy(),
            rpm_name(env!("CARGO_PKG_VERSION"))
        );

        let contents = Command::new("rpm")
            .args(["-qpl", archive.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(contents.status.success());
        let contents = String::from_utf8_lossy(&contents.stdout);
        for path in [
            "/opt/arro/arro",
            "/opt/arro/helpers/one.jar",
            "/opt/arro/tools/engine/server.jar",
            "/usr/bin/arro",
            "/usr/share/applications/arro.desktop",
            "/usr/share/icons/hicolor/16x16/apps/arro.png",
        ] {
            assert!(contents.contains(path), "{path} is not in {contents}");
        }
        // Only the folder of the program is a directory of the package; the folders of
        // the system belong to the packages that already own them.
        assert!(
            contents.lines().any(|line| line == "/opt/arro"),
            "{contents}"
        );
        for path in ["/opt", "/usr", "/usr/bin", "/usr/share"] {
            assert!(
                !contents.lines().any(|line| line == path),
                "{path} is not a folder of this package: {contents}"
            );
        }
    }

    #[cfg(unix)]
    fn run_installer(script: &Path, home: &Path, args: &[&std::ffi::OsStr]) -> (String, String) {
        let output = Command::new("sh")
            .arg(script)
            .args(args)
            .env("HOME", home)
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// Without root the script copies the folder into the home directory, links the
    /// program, writes the desktop entry and the icons, and `--uninstall` takes all of it
    /// away. A second call copies into the folder it is given.
    #[cfg(unix)]
    #[test]
    fn install_sh_installs_into_the_home_folder_and_uninstalls() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new("dist-linux-install");
        let archive = temp.path().join("arro-0.1.0-linux-x86_64");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        release_tree(&temp.path().join("repo"), &archive);
        // The installer takes every render in the folder; the release of the test
        // carries both of the resources.
        fs::write(archive.join("arro-16.png"), b"png16").unwrap();
        let installer = archive.join("install.sh");

        let (stdout, stderr) = run_installer(&installer, &home, &[]);
        assert!(stdout.contains("Installed Arro"), "{stdout}{stderr}");

        let installed = home.join(".local/share/arro");
        assert_eq!(fs::read(installed.join("arro")).unwrap(), b"program");
        assert!(installed.join("helpers/one.jar").is_file());
        assert!(installed.join("tools/engine/server.jar").is_file());
        assert_eq!(
            fs::metadata(installed.join("arro"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let link = home.join(".local/bin/arro");
        assert!(link_points_to(&link, &installed.join("arro")));

        let entry =
            fs::read_to_string(home.join(".local/share/applications/arro.desktop")).unwrap();
        assert!(
            entry.contains(&format!("Exec=\"{}/arro\" %F", installed.display())),
            "{entry}"
        );
        assert!(entry.contains("MimeType=inode/directory;"), "{entry}");
        assert!(!entry.contains("@ARRO_DIR@"), "{entry}");
        assert!(
            home.join(".local/share/icons/hicolor/16x16/apps/arro.png")
                .is_file()
        );
        assert!(
            home.join(".local/share/icons/hicolor/256x256/apps/arro.png")
                .is_file()
        );

        let (stdout, stderr) = run_installer(&installer, &home, &[OsStr::new("--uninstall")]);
        assert!(stdout.contains("Removed"), "{stdout}{stderr}");
        assert!(fs::symlink_metadata(&installed).is_err());
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(fs::symlink_metadata(home.join(".local/share/applications/arro.desktop")).is_err());
        assert!(
            fs::symlink_metadata(home.join(".local/share/icons/hicolor/16x16/apps/arro.png"))
                .is_err()
        );

        // The folder from the argument wins over the default, and the same argument is
        // what `--uninstall` removes.
        let custom = home.join("custom arro");
        let (stdout, stderr) = run_installer(&installer, &home, &[custom.as_os_str()]);
        assert!(stdout.contains("Installed Arro"), "{stdout}{stderr}");
        assert!(custom.join("arro").is_file());
        assert!(link_points_to(&link, &custom.join("arro")));
        let entry =
            fs::read_to_string(home.join(".local/share/applications/arro.desktop")).unwrap();
        assert!(entry.contains(&*custom.to_string_lossy()), "{entry}");

        let (stdout, stderr) = run_installer(
            &installer,
            &home,
            &[OsStr::new("--uninstall"), custom.as_os_str()],
        );
        assert!(stdout.contains("Removed"), "{stdout}{stderr}");
        assert!(fs::symlink_metadata(&custom).is_err());
        assert!(fs::symlink_metadata(&link).is_err());
    }

    /// A folder inside the unpacked archive would copy the copy into itself.
    #[cfg(unix)]
    #[test]
    fn install_sh_refuses_a_folder_inside_the_archive() {
        let temp = TempDir::new("dist-linux-inside");
        let archive = temp.path().join("arro-0.1.0-linux-x86_64");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        release_tree(&temp.path().join("repo"), &archive);

        let inside = archive.join("installed");
        let (stdout, stderr) =
            run_installer(&archive.join("install.sh"), &home, &[inside.as_os_str()]);
        assert!(stderr.contains("inside the archive"), "{stdout}{stderr}");
        assert!(!inside.exists());
    }

    /// The script refuses a folder without the program instead of removing it.
    #[cfg(unix)]
    #[test]
    fn install_sh_uninstall_leaves_a_foreign_folder_alone() {
        let temp = TempDir::new("dist-linux-uninstall");
        let archive = temp.path().join("arro-0.1.0-linux-x86_64");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        release_tree(&temp.path().join("repo"), &archive);
        let foreign = home.join("documents");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("note.txt"), b"keep").unwrap();

        let (stdout, stderr) = run_installer(
            &archive.join("install.sh"),
            &home,
            &[OsStr::new("--uninstall"), foreign.as_os_str()],
        );
        assert!(stderr.contains("is not an Arro folder"), "{stdout}{stderr}");
        assert_eq!(fs::read(foreign.join("note.txt")).unwrap(), b"keep");
    }
}
