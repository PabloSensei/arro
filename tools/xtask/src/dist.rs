//! Assembles the zip distribution: the release binary, helper jars, the launch script and
//! the notices. Release and dist stay out of `ci`, which has no JVM and no need for them.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{Result, cargo, conpty, helpers, notice};

const EXE: &str = "arro.exe";
const PDB: &str = "arro.pdb";
const PLATFORM: &str = "x86_64-pc-windows-msvc";
const README_TEMPLATE: &str = include_str!("../../../dist/README.txt");
const RUN_ARRO_TEMPLATE: &str = include_str!("../../../dist/run-arro.cmd");
/// The Eclipse Public License 1.0 as published at https://www.eclipse.org/legal/epl-v10.html.
/// The java-debug archive declares this license but carries no copy of it.
pub(crate) const EPL_1_0: &str = include_str!("../../../dist/licenses/EPL-1.0.txt");

/// The pinned archives of the language tools. The versions and hashes are part of
/// the release, as they are for ConPTY; a different file is an error, not a warning.
const JDTLS_ARCHIVE: &str = "jdt-language-server-1.63.0-202610191322.tar.gz";
const JDTLS_VERSION: &str = "1.63.0";
const JDTLS_SHA256: &str = "b1f2abe0b3cccb462cf64fb4a5587c7d09df60cf8be7748b152cbd89d018bdc4";
const JDTLS_SOURCE: &str = "https://github.com/eclipse-jdtls/eclipse.jdt.ls";
const JDTLS_DOWNLOAD: &str = "https://download.eclipse.org/jdtls/snapshots/";
const DEBUG_ARCHIVE: &str = "vscjava.vscode-java-debug-0.60.0.vsix";
const DEBUG_VERSION: &str = "0.60.0";
const DEBUG_PLUGIN_VERSION: &str = "0.53.2";
const DEBUG_SHA256: &str = "f09ebe358a7b08b3c17b21b8d8207b0649acad120eb6f1604dc4f15379f59d66";
const DEBUG_SOURCE: &str = "https://github.com/microsoft/java-debug";
const DEBUG_DOWNLOAD: &str =
    "https://marketplace.visualstudio.com/items?itemName=vscjava.vscode-java-debug";

/// What the language tools take in the output folder.
pub(crate) struct JavaTools {
    pub(crate) jdtls: PathBuf,
    pub(crate) debug: PathBuf,
}

pub fn run(
    root: &Path,
    out: &Path,
    symbols: Option<&Path>,
    tools: Option<&Path>,
    zip: bool,
) -> Result {
    let out = absolute(out)?;
    let root = absolute(root)?;
    if out == root {
        return Err("refusing to write the distribution into the workspace root".into());
    }
    if out == root.join("dist") {
        return Err("dist is the template folder, choose another output directory".into());
    }
    let tools = tools_directory(tools)?;
    let symbols = match symbols {
        Some(dir) => absolute(dir)?,
        None => default_symbols_dir(&out),
    };
    fs::create_dir_all(&out)?;
    fs::create_dir_all(&symbols)?;
    if fs::canonicalize(&symbols)?.starts_with(fs::canonicalize(&out)?) {
        return Err(format!(
            "the symbols folder {} must be outside the distribution {}",
            symbols.display(),
            out.display()
        )
        .into());
    }
    remove_old(&out)?;

    build_release(&root)?;
    let target = target_directory(&root)?;
    let exe = target.join("release").join(EXE);
    if !exe.is_file() {
        return Err(format!("{} was not built", exe.display()).into());
    }
    let copied = out.join(EXE);
    fs::copy(&exe, &copied)?;
    println!("{}", copied.display());

    let conpty = conpty::install(&root, &out)?;

    let helpers_dir = out.join("helpers");
    helpers::run(&root, &helpers_dir, None)?;

    let java_tools = install_java_tools(&out, &tools, JDTLS_SHA256, DEBUG_SHA256)?;

    let commit = git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let date = git(&root, &["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    let saved = copy_symbols(&target.join("release"), &symbols, &commit)?;
    println!("symbols: {}", saved.display());
    let readme = README_TEMPLATE
        .replace("{{COMMIT}}", &commit)
        .replace("{{DATE}}", &date);
    write_text(&out.join("README.txt"), &readme)?;
    write_text(&out.join("run-arro.cmd"), RUN_ARRO_TEMPLATE)?;
    write_text(&out.join("THIRD-PARTY.txt"), &third_party(&root)?)?;
    let mut notice = notice::render(conpty);
    notice.push_str(&java_notice(&java_tools));
    write_text(&out.join("NOTICE.txt"), &notice)?;
    write_text(
        &java_tools.debug.join("extension").join("EPL-1.0.txt"),
        EPL_1_0,
    )?;

    println!("dist: {} (commit {commit}, {date})", out.display());
    if zip {
        let archive = zip_folder(&out)?;
        println!("{}", archive.display());
    }
    Ok(())
}

/// The folder next to the distribution: `<out>-symbols`.
fn default_symbols_dir(out: &Path) -> PathBuf {
    match out.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push("-symbols");
            out.with_file_name(name)
        }
        None => {
            let mut path = out.as_os_str().to_owned();
            path.push("-symbols");
            PathBuf::from(path)
        }
    }
}

/// `pdb` is far too big for the archive, and a user does not need it. It is kept by
/// commit next to a copy of the same executable, so `symbolize` reads a log of any
/// release with its own symbols.
fn copy_symbols(release: &Path, root: &Path, commit: &str) -> Result<PathBuf> {
    let pdb = release.join(PDB);
    if !pdb.is_file() {
        return Err(format!("{} was not built", pdb.display()).into());
    }
    let dir = root.join(commit);
    fs::create_dir_all(&dir)?;
    fs::copy(release.join(EXE), dir.join(EXE))?;
    fs::copy(&pdb, dir.join(PDB))?;
    Ok(dir)
}

pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

/// Only the files this command writes are replaced. Anything else in the folder stays.
fn remove_old(out: &Path) -> Result {
    for name in [
        EXE,
        "README.txt",
        "run-arro.cmd",
        "THIRD-PARTY.txt",
        "NOTICE.txt",
        "conpty.dll",
        "OpenConsole.exe",
        PDB,
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

/// The folder with `download-jdtls` and `download-java-debug`: `--tools DIR` or
/// `$IDE_CORPUS/tools`.
pub(crate) fn tools_directory(tools: Option<&Path>) -> Result<PathBuf> {
    match tools {
        Some(dir) => absolute(dir),
        None => {
            let corpus = env::var_os("IDE_CORPUS").ok_or(
                "set IDE_CORPUS to the folder with tools/download-jdtls and \
                 tools/download-java-debug, or pass --tools DIR",
            )?;
            Ok(PathBuf::from(corpus).join("tools"))
        }
    }
}

/// Unpacks the two pinned archives into `<out>\tools` in the layout `setup.rs` and
/// `run-arro.cmd` search: `jdtls-<version>` and
/// `vscode-java-debug-<version>\extension\server\...jar`.
pub(crate) fn install_java_tools(
    out: &Path,
    tools: &Path,
    jdtls_sha256: &str,
    debug_sha256: &str,
) -> Result<JavaTools> {
    install_java_tools_for(out, tools, jdtls_sha256, debug_sha256, "config_win")
}

/// The macOS bundle keeps the servers inside `Contents/Resources/tools`, with the
/// configuration folder `config_mac`.
pub(crate) fn install_java_tools_macos(out: &Path, tools: &Path) -> Result<JavaTools> {
    install_java_tools_for(out, tools, JDTLS_SHA256, DEBUG_SHA256, "config_mac")
}

/// The Linux folder keeps `tools` next to the program, with the configuration folder
/// `config_linux`.
pub(crate) fn install_java_tools_linux(out: &Path, tools: &Path) -> Result<JavaTools> {
    install_java_tools_for(out, tools, JDTLS_SHA256, DEBUG_SHA256, "config_linux")
}

/// The same servers with the configuration folder of another system, which is all that
/// differs between the releases of jdtls.
pub(crate) fn install_java_tools_for(
    out: &Path,
    tools: &Path,
    jdtls_sha256: &str,
    debug_sha256: &str,
    config: &str,
) -> Result<JavaTools> {
    let jdtls_archive = tools.join("download-jdtls").join(JDTLS_ARCHIVE);
    let debug_archive = tools.join("download-java-debug").join(DEBUG_ARCHIVE);
    verify_archive(
        &jdtls_archive,
        jdtls_sha256,
        JDTLS_DOWNLOAD,
        "Eclipse JDT Language Server",
    )?;
    verify_archive(
        &debug_archive,
        debug_sha256,
        DEBUG_DOWNLOAD,
        "vscode-java-debug",
    )?;

    let base = out.join("tools");
    fs::create_dir_all(&base)?;
    let jdtls = base.join(format!("jdtls-{JDTLS_VERSION}"));
    let debug = base.join(format!("vscode-java-debug-{DEBUG_VERSION}"));
    unpack_jdtls(out, &jdtls_archive, &jdtls, config)?;
    unpack_debug(out, &debug_archive, &debug)?;
    println!("dist: {}", jdtls.display());
    println!("dist: {}", debug.display());
    Ok(JavaTools { jdtls, debug })
}

/// A missing or different archive stops `dist`: the release must name the file the
/// user has to fetch, not ship a different server.
pub(crate) fn verify_archive(path: &Path, expected: &str, download: &str, what: &str) -> Result {
    if !path.is_file() {
        return Err(format!(
            "{} is missing. Download {what} and put the archive there: {download}",
            path.display()
        )
        .into());
    }
    let actual = conpty::sha256_file(path)?;
    if actual != expected.to_ascii_lowercase() {
        return Err(format!(
            "{}: SHA-256 {actual} does not match the pinned {expected}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn unpack_jdtls(out: &Path, archive: &Path, target: &Path, config: &str) -> Result {
    let unpacked = out.join(format!(".unpack-jdtls-{}", std::process::id()));
    let _ = fs::remove_dir_all(&unpacked);
    let result = (|| -> Result {
        unpack(archive, &unpacked)?;
        // The release keeps the full EPL text in the launcher feature. It joins the
        // license files of the root, so the notice can read it from the distribution.
        extract_epl(&unpacked);
        fs::create_dir_all(target)?;
        for name in ["bin", "features", "plugins", config] {
            let from = unpacked.join(name);
            if from.is_dir() {
                fs::rename(&from, target.join(name))?;
            }
        }
        for entry in fs::read_dir(&unpacked)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if path.is_file() && is_license_file(name) {
                fs::rename(&path, target.join(name))?;
            }
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&unpacked);
    result
}

fn unpack_debug(out: &Path, archive: &Path, target: &Path) -> Result {
    let unpacked = out.join(format!(".unpack-debug-{}", std::process::id()));
    let _ = fs::remove_dir_all(&unpacked);
    let result = (|| -> Result {
        unpack(archive, &unpacked)?;
        let extension = unpacked.join("extension");
        let server = target.join("extension").join("server");
        fs::create_dir_all(&server)?;
        let license = extension.join("LICENSE.txt");
        if license.is_file() {
            fs::copy(&license, target.join("extension").join("LICENSE.txt"))?;
        }
        let mut jars: Vec<PathBuf> = fs::read_dir(extension.join("server"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                path.is_file()
                    && name.starts_with("com.microsoft.java.debug.plugin-")
                    && name.ends_with(".jar")
            })
            .collect();
        jars.sort();
        let jar = jars
            .pop()
            .ok_or("the java-debug archive has no com.microsoft.java.debug.plugin-*.jar")?;
        let name = jar.file_name().ok_or("the java-debug jar has no name")?;
        fs::copy(&jar, server.join(name))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&unpacked);
    result
}

/// Unpacks `archive`. The Windows `tar.exe` reads both `.tar.gz` and the zip inside a
/// `.vsix`; GNU tar on Linux reads no zip, so a zip goes to `unzip` outside Windows.
pub(crate) fn unpack(archive: &Path, directory: &Path) -> Result {
    fs::create_dir_all(directory)?;
    let mut command = if is_zip(archive) && !cfg!(windows) {
        let mut command = Command::new("unzip");
        command
            .args(["-q", "-o"])
            .arg(archive)
            .arg("-d")
            .arg(directory);
        command
    } else {
        let mut command = Command::new(tar());
        command
            .args(["-x", "-f"])
            .arg(archive)
            .arg("-C")
            .arg(directory);
        command
    };
    check(&mut command)
}

/// A `.zip`, `.vsix` or `.jar`: an archive in the zip format.
fn is_zip(archive: &Path) -> bool {
    archive
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["zip", "vsix", "jar"]
                .iter()
                .any(|zip| extension.eq_ignore_ascii_case(zip))
        })
}

/// Reads `epl-2.0.html` out of the Equinox launcher feature into the root of the
/// unpacked release. A release without that file is not an error: the notice then
/// points at the EPL page instead.
fn extract_epl(unpacked: &Path) {
    let features = unpacked.join("features");
    let Ok(entries) = fs::read_dir(&features) else {
        return;
    };
    let Some(feature) = entries.flatten().map(|entry| entry.path()).find(|path| {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        name.starts_with("org.eclipse.equinox.executable_") && name.ends_with(".jar")
    }) else {
        return;
    };
    let mut command = if cfg!(windows) {
        let mut command = Command::new(tar());
        command
            .args(["-x", "-f"])
            .arg(feature)
            .arg("-C")
            .arg(unpacked)
            .arg("epl-2.0.html");
        command
    } else {
        let mut command = Command::new("unzip");
        command
            .args(["-q", "-o"])
            .arg(feature)
            .arg("epl-2.0.html")
            .arg("-d")
            .arg(unpacked);
        command
    };
    let _ = command.output();
}

/// The license files of the archive root, such as `LICENSE` and `NOTICE`. The release
/// keeps the rest of its root next to them.
fn is_license_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["license", "notice", "copying", "epl-", "about"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

pub(crate) fn build_release(root: &Path) -> Result {
    let mut command = Command::new(cargo());
    command
        .args(["build", "--release", "--package", "ide"])
        .current_dir(root);
    check(&mut command)
}

pub(crate) fn target_directory(root: &Path) -> Result<PathBuf> {
    let metadata = metadata(root, &["--no-deps"])?;
    let target = metadata["target_directory"]
        .as_str()
        .ok_or("cargo metadata has no target directory")?;
    Ok(PathBuf::from(target))
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Option<String> {
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

pub(crate) fn write_text(path: &Path, text: &str) -> Result {
    let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    fs::write(path, text)?;
    Ok(())
}

/// Writes a text with LF endings, as a Linux release carries it.
pub(crate) fn write_unix_text(path: &Path, text: &str) -> Result {
    fs::write(path, text.replace("\r\n", "\n"))?;
    Ok(())
}

/// Sets the executable bits of a file of a Linux or macOS release. Windows has no
/// such bits, and a package built there keeps the file as it is.
pub(crate) fn make_executable(path: &Path) -> Result {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The archive sits next to the folder and holds the folder itself, as Windows `tar`
/// writes it. The command is a Windows tool, so no new dependency is needed.
fn zip_folder(out: &Path) -> Result<PathBuf> {
    let parent = out.parent().ok_or("the output folder has no parent")?;
    let name = out.file_name().ok_or("the output folder has no name")?;
    let mut archive_name = name.to_os_string();
    archive_name.push(".zip");
    let archive = parent.join(archive_name);
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let mut command = Command::new(tar());
    command
        .args(["-a", "-c", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(parent)
        .arg(name);
    check(&mut command)?;
    Ok(archive)
}

/// The Windows `tar.exe` understands `-a` and writes a zip. A `tar` from a PATH full of
/// Unix tools may not, so the one under `SystemRoot` comes first.
pub(crate) fn tar() -> PathBuf {
    if let Some(root) = env::var_os("SystemRoot") {
        let tool = PathBuf::from(root).join("System32").join("tar.exe");
        if tool.is_file() {
            return tool;
        }
    }
    PathBuf::from("tar")
}

pub(crate) fn check(command: &mut Command) -> Result {
    let status = command.status()?;
    if status.success() {
        return Ok(());
    }
    Err(format!("{:?} failed: {status}", command.get_program()).into())
}

fn metadata(root: &Path, extra: &[&str]) -> Result<Value> {
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1"])
        .args(extra)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Every third-party crate in the graph of `ide`, grouped by the license expression that
/// `cargo metadata` reports. Workspace members are our own code and stay out.
pub(crate) fn third_party(root: &Path) -> Result<String> {
    third_party_for(root, PLATFORM)
}

/// The same list for the graph of another system, which carries other crates.
pub(crate) fn third_party_for(root: &Path, platform: &str) -> Result<String> {
    let metadata = metadata(root, &["--filter-platform", platform])?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?;
    let ide = packages
        .iter()
        .find(|package| package["name"] == "ide")
        .ok_or("cargo metadata has no ide package")?;
    let ide = ide["id"].as_str().ok_or("ide has no id")?.to_owned();
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();

    let mut seen = BTreeSet::new();
    let mut stack = vec![ide];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(id.as_str()) else {
            continue;
        };
        if let Some(deps) = node["deps"].as_array() {
            for dep in deps {
                let linked = dep["dep_kinds"].as_array().is_none_or(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind["kind"].as_str() != Some("dev"))
                });
                if linked && let Some(package) = dep["pkg"].as_str() {
                    stack.push(package.to_owned());
                }
            }
        } else {
            for package in node["dependencies"].as_array().into_iter().flatten() {
                if let Some(package) = package.as_str() {
                    stack.push(package.to_owned());
                }
            }
        }
    }

    let by_id: BTreeMap<&str, &Value> = packages
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package)))
        .collect();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in &seen {
        if members.contains(id.as_str()) {
            continue;
        }
        let Some(package) = by_id.get(id.as_str()) else {
            continue;
        };
        let name = package["name"].as_str().unwrap_or("unknown");
        let version = package["version"].as_str().unwrap_or("unknown");
        let license = package["license"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                package["license_file"]
                    .as_str()
                    .map(|file| format!("see {file}"))
            })
            .ok_or_else(|| format!("{name} {version} reports no license"))?;
        groups
            .entry(license)
            .or_default()
            .push(format!("{name} {version}"));
    }
    Ok(render_third_party(&groups))
}

fn render_third_party(groups: &BTreeMap<String, Vec<String>>) -> String {
    let mut text = String::new();
    text.push_str("THIRD-PARTY SOFTWARE\n");
    text.push_str("====================\n\n");
    text.push_str(
        "The arro.exe binary contains the Rust crates below. They are grouped by the license\n",
    );
    text.push_str("expression reported by cargo metadata for ");
    text.push_str(PLATFORM);
    text.push_str(".\n\n");
    for (license, crates) in groups {
        let mut crates = crates.clone();
        crates.sort();
        text.push_str(license);
        text.push_str(&format!(
            " ({} {})\n",
            crates.len(),
            if crates.len() == 1 { "crate" } else { "crates" }
        ));
        text.push_str(&"-".repeat(license.len().max(10)));
        text.push('\n');
        for krate in crates {
            text.push_str("  ");
            text.push_str(&krate);
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str("BUNDLED JAVA TOOLS\n");
    text.push_str("==================\n\n");
    text.push_str(&format!(
        "jdtls {JDTLS_VERSION}, the Eclipse JDT Language Server, is bundled in\n\
         tools\\jdtls-{JDTLS_VERSION}. It is licensed under EPL-2.0; the full text is in\n\
         tools\\jdtls-{JDTLS_VERSION}\\epl-2.0.html. Sources: {JDTLS_SOURCE}\n\n"
    ));
    text.push_str(&format!(
        "java-debug {DEBUG_VERSION} is bundled in tools\\vscode-java-debug-{DEBUG_VERSION}.\n\
         The com.microsoft.java.debug.plugin jar {DEBUG_PLUGIN_VERSION} is licensed under\n\
         EPL-1.0; the extension around it is MIT-licensed. Its dependencies include\n\
         commons-io (Apache-2.0), RxJava (Apache-2.0) and Reactive Streams (CC0-1.0).\n\
         Sources: {DEBUG_SOURCE}\n\n"
    ));
    text.push_str("BUNDLED ASSETS\n");
    text.push_str("==============\n\n");
    text.push_str(
        "The binary also embeds assets that are not Rust crates:\n\n\
         \x20 Inter 4.1, SIL Open Font License 1.1;\n\
         \x20 JetBrains Mono 2.304, SIL Open Font License 1.1;\n\
         \x20 icons built from Lucide 1.70.0 and 1.43.0, ISC (in part MIT, from Feather);\n\
         \x20 the default keymap from JetBrains intellij-community, Apache-2.0.\n\n\
         conpty.dll and OpenConsole.exe, when they are shipped next to arro.exe, come from\n\
         Microsoft Windows Terminal and are MIT-licensed; NOTICE.txt holds that text.\n\n\
         NOTICE.txt holds the attribution, the font licenses and the Apache License 2.0.\n\
         IntelliJ IDEA and JetBrains are trademarks of JetBrains s.r.o.; this project is\n\
         not affiliated with JetBrains and contains no product logos.\n",
    );
    text
}

/// The part of `NOTICE.txt` about the bundled language tools. The full texts come
/// from the archives themselves: the EPL-2.0 page extracted from the launcher feature
/// and the MIT license of the java-debug extension.
pub(crate) fn java_notice(tools: &JavaTools) -> String {
    let mut text = String::new();
    text.push_str("\nJava language tools\n-------------------\n\n");
    text.push_str(&format!(
        "jdtls {JDTLS_VERSION}, the Eclipse JDT Language Server, is bundled in\n\
         tools\\jdtls-{JDTLS_VERSION}. It is licensed under the Eclipse Public License 2.0\n\
         (EPL-2.0). Sources: {JDTLS_SOURCE}\n\n"
    ));
    let epl = tools.jdtls.join("epl-2.0.html");
    match fs::read_to_string(&epl) {
        Ok(license) => {
            text.push_str("The full EPL-2.0 text from the release archive follows.\n\n");
            text.push_str(&license);
            text.push('\n');
        }
        Err(_) => {
            text.push_str(
                "The full EPL-2.0 text is available at https://www.eclipse.org/legal/epl-2.0/.\n\n",
            );
        }
    }
    text.push_str(&format!(
        "\njava-debug {DEBUG_VERSION} is bundled in\n\
         tools\\vscode-java-debug-{DEBUG_VERSION}. The com.microsoft.java.debug.plugin jar\n\
         {DEBUG_PLUGIN_VERSION} declares Eclipse Public License 1.0 (EPL-1.0) in its\n\
         manifest. Sources: {DEBUG_SOURCE}\n\n\
         The release archive carries no copy of that license; the full EPL-1.0 text from\n\
         https://www.eclipse.org/legal/epl-v10.html follows and is also in\n\
         tools\\vscode-java-debug-{DEBUG_VERSION}\\extension\\EPL-1.0.txt.\n\n"
    ));
    text.push_str(EPL_1_0);
    text.push('\n');
    let license = tools.debug.join("extension").join("LICENSE.txt");
    match fs::read_to_string(&license) {
        Ok(license) => {
            text.push_str(
                "The vscode-java-debug extension around the plugin is MIT-licensed; the text\n\
                 from the archive follows.\n\n",
            );
            text.push_str(&license);
            text.push('\n');
        }
        Err(_) => {
            text.push_str(
                "The vscode-java-debug extension around the plugin is MIT-licensed.\n\
                 See https://github.com/microsoft/vscode-java-debug\n",
            );
        }
    }
    text
}

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::process::CommandExt;

    use test_support::TempDir;

    trait Script {
        /// The launcher script of the test, inside the folder of the test.
        fn write_script(&self) -> PathBuf;
    }

    impl Script for TempDir {
        fn write_script(&self) -> PathBuf {
            let script = self.path().join("run-arro.cmd");
            write_text(&script, RUN_ARRO_TEMPLATE).unwrap();
            script
        }
    }

    fn tool_layout(root: &Path) -> (PathBuf, PathBuf) {
        let jdtls = root.join("tools").join("jdtls-1.2.3");
        fs::create_dir_all(jdtls.join("plugins")).unwrap();
        let plugin = root
            .join("tools")
            .join("vscode-java-debug-0.53.1")
            .join("extension")
            .join("server")
            .join("com.microsoft.java.debug.plugin-0.53.1.jar");
        fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        fs::write(&plugin, b"").unwrap();
        (jdtls, plugin)
    }

    /// A JDK folder the script can identify: the version lives in `release`, as in a
    /// real installation.
    fn jdk(dir: &Path, version: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("release"),
            format!("IMPLEMENTOR=\"test\"\nJAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
        dir.to_owned()
    }

    fn run_script(script: &Path, envs: &[(&str, &OsStr)], removed: &[&str]) -> (String, String) {
        let mut command = Command::new("cmd");
        command
            .arg("/D")
            .arg("/C")
            .raw_arg(format!("\"{}\" --print-paths", script.display()));
        for name in removed {
            command.env_remove(name);
        }
        for (name, value) in envs {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        (
            String::from_utf8_lossy(&output.stdout).replace('\r', ""),
            String::from_utf8_lossy(&output.stderr).replace('\r', ""),
        )
    }

    const REMOVED: &[&str] = &[
        "IDE_JAVA_HOME",
        "IDE_JDTLS_HOME",
        "IDE_JAVA_DEBUG_PLUGIN",
        "IDE_TOOLS",
        "IDE_PROGRAM_FILES",
        "JAVA_HOME",
        "LOCALAPPDATA",
        "ProgramFiles",
        "PROGRAMFILES",
        "ProgramFiles(x86)",
    ];

    #[test]
    fn finds_tools_next_to_the_binary() {
        let temp = TempDir::new("beside");
        let java_home = jdk(&temp.path().join("jdk-25"), "25.0.5");
        let (jdtls, plugin) = tool_layout(temp.path());
        let empty = temp.path().join("ProgramFiles");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("JAVA_HOME", java_home.as_os_str()),
                ("IDE_PROGRAM_FILES", empty.as_os_str()),
            ],
            REMOVED,
        );
        assert_eq!(
            stdout,
            format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME={}\nSTATUS=ready\n",
                java_home.display(),
                jdtls.display(),
                plugin.display(),
                java_home.display()
            ),
            "{stderr}"
        );
    }

    #[test]
    fn finds_tools_in_ide_tools_and_uses_the_newest_jdk() {
        let temp = TempDir::new("tools with space");
        let tools = temp.path().join("third-party");
        let (jdtls, plugin) = tool_layout(&tools);
        let program_files = temp.path().join("ProgramFiles");
        let corretto = program_files.join("Amazon Corretto");
        jdk(&corretto.join("jdk-8.0.412_08"), "1.8.0_492");
        jdk(&corretto.join("jdk17.0.9_9"), "17.0.9");
        let new = jdk(&corretto.join("jdk25.0.5_11"), "25.0.5");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_TOOLS", tools.as_os_str()),
                ("IDE_PROGRAM_FILES", program_files.as_os_str()),
            ],
            REMOVED,
        );
        assert_eq!(
            stdout,
            format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME=\nSTATUS=ready\n",
                new.display(),
                jdtls.display(),
                plugin.display()
            ),
            "{stderr}"
        );
    }

    #[test]
    fn environment_variables_win_over_the_search() {
        let temp = TempDir::new("overrides");
        let jdk = jdk(&temp.path().join("custom-jdk"), "25.0.4");
        let jdtls = temp.path().join("custom-jdtls");
        let plugin = temp.path().join("custom-debug.jar");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_JAVA_HOME", jdk.as_os_str()),
                ("IDE_JDTLS_HOME", jdtls.as_os_str()),
                ("IDE_JAVA_DEBUG_PLUGIN", plugin.as_os_str()),
            ],
            REMOVED,
        );
        assert_eq!(
            stdout,
            format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME=\nSTATUS=ready\n",
                jdk.display(),
                jdtls.display(),
                plugin.display()
            ),
            "{stderr}"
        );
    }

    #[test]
    fn without_jdtls_it_reports_one_line_and_still_lists_the_paths() {
        let temp = TempDir::new("missing");
        let program_files = temp.path().join("ProgramFiles");
        let java = jdk(&program_files.join("Java").join("jdk-25"), "25.0.1");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[("IDE_PROGRAM_FILES", program_files.as_os_str())],
            REMOVED,
        );
        assert!(
            stdout.contains(&format!("IDE_JAVA_HOME={}\n", java.display())),
            "{stdout}{stderr}"
        );
        assert!(stdout.contains("IDE_JDTLS_HOME=\n"), "{stdout}{stderr}");
        assert!(stdout.contains("JAVA_HOME=\n"), "{stdout}{stderr}");
        assert!(stdout.contains("STATUS=no-jdtls\n"), "{stdout}{stderr}");
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("run-arro:"))
                .count(),
            1,
            "{stdout}{stderr}"
        );
        assert!(stdout.contains("jdtls not found"), "{stdout}{stderr}");
    }

    #[test]
    fn java_home_17_is_skipped_for_corretto_25_and_left_unchanged() {
        let temp = TempDir::new("java-home-old");
        let tools = temp.path().join("tools");
        let (jdtls, plugin) = tool_layout(&tools);
        let program_files = temp.path().join("ProgramFiles");
        let corretto = jdk(
            &program_files.join("Amazon Corretto").join("jdk25.0.5_11"),
            "25.0.5",
        );
        let java_home = jdk(&temp.path().join("jdk-17.0.20.1"), "17.0.20.1");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_TOOLS", tools.as_os_str()),
                ("IDE_PROGRAM_FILES", program_files.as_os_str()),
                ("JAVA_HOME", java_home.as_os_str()),
            ],
            REMOVED,
        );
        assert_eq!(
            stdout,
            format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME={}\nSTATUS=ready\n",
                corretto.display(),
                jdtls.display(),
                plugin.display(),
                java_home.display()
            ),
            "{stderr}"
        );
    }

    #[test]
    fn without_a_jdk_25_it_starts_without_jdtls() {
        let temp = TempDir::new("no-jdk-25");
        let tools = temp.path().join("tools");
        tool_layout(&tools);
        let program_files = temp.path().join("ProgramFiles");
        jdk(
            &program_files.join("Amazon Corretto").join("jdk-8.0.412_08"),
            "1.8.0_492",
        );
        let java_home = jdk(&temp.path().join("jdk-17.0.20.1"), "17.0.20.1");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_TOOLS", tools.as_os_str()),
                ("IDE_PROGRAM_FILES", program_files.as_os_str()),
                ("JAVA_HOME", java_home.as_os_str()),
            ],
            REMOVED,
        );
        assert!(stdout.contains("IDE_JAVA_HOME=\n"), "{stdout}{stderr}");
        assert!(
            stdout.contains(&format!("JAVA_HOME={}\n", java_home.display())),
            "{stdout}{stderr}"
        );
        assert!(stdout.contains("STATUS=no-jdtls\n"), "{stdout}{stderr}");
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("run-arro:"))
                .count(),
            1,
            "{stdout}{stderr}"
        );
        assert!(stdout.contains("JDK 25 or newer"), "{stdout}{stderr}");
    }

    #[test]
    fn ide_java_home_below_25_is_used_with_one_warning() {
        let temp = TempDir::new("explicit-old-jdk");
        let tools = temp.path().join("tools");
        let (jdtls, plugin) = tool_layout(&tools);
        let chosen = jdk(&temp.path().join("jdk-17.0.20.1"), "17.0.20.1");
        let program_files = temp.path().join("ProgramFiles");
        jdk(
            &program_files.join("Amazon Corretto").join("jdk25.0.5_11"),
            "25.0.5",
        );
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_JAVA_HOME", chosen.as_os_str()),
                ("IDE_TOOLS", tools.as_os_str()),
                ("IDE_PROGRAM_FILES", program_files.as_os_str()),
            ],
            REMOVED,
        );
        assert!(
            stdout.contains(&format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME=\n",
                chosen.display(),
                jdtls.display(),
                plugin.display()
            )),
            "{stdout}{stderr}"
        );
        let warnings: Vec<&str> = stdout
            .lines()
            .filter(|line| line.starts_with("run-arro:"))
            .collect();
        assert_eq!(warnings.len(), 1, "{stdout}{stderr}");
        assert!(warnings[0].contains("IDE_JAVA_HOME"), "{stdout}{stderr}");
        assert!(warnings[0].contains("25 or newer"), "{stdout}{stderr}");
        assert!(stdout.contains("STATUS=ready\n"), "{stdout}{stderr}");
    }

    #[test]
    fn searches_adoptium_and_local_programs_for_the_newest_jdk() {
        let temp = TempDir::new("more-roots");
        let tools = temp.path().join("tools");
        let (jdtls, plugin) = tool_layout(&tools);
        let program_files = temp.path().join("ProgramFiles");
        jdk(
            &program_files.join("Eclipse Adoptium").join("jdk-25.0.5"),
            "25.0.5",
        );
        let local = temp.path().join("LocalAppData");
        let newest = jdk(&local.join("Programs").join("jdk-26"), "26.0.1");
        let script = temp.write_script();
        let (stdout, stderr) = run_script(
            &script,
            &[
                ("IDE_TOOLS", tools.as_os_str()),
                ("IDE_PROGRAM_FILES", program_files.as_os_str()),
                ("LOCALAPPDATA", local.as_os_str()),
            ],
            REMOVED,
        );
        assert_eq!(
            stdout,
            format!(
                "IDE_JAVA_HOME={}\nIDE_JDTLS_HOME={}\nIDE_JAVA_DEBUG_PLUGIN={}\nJAVA_HOME=\nSTATUS=ready\n",
                newest.display(),
                jdtls.display(),
                plugin.display()
            ),
            "{stderr}"
        );
    }

    fn archive(tree: &Path, archive: &Path, zip: bool) {
        let mut command = Command::new(tar());
        if zip {
            command.args(["-a", "-c", "-f"]);
        } else {
            command.args(["-c", "-z", "-f"]);
        }
        command.arg(archive).arg("-C").arg(tree).arg(".");
        assert!(command.status().unwrap().success(), "{}", archive.display());
    }

    /// The release archives of the test: the same folder names inside, a different
    /// version in the names of the files.
    fn fake_archives(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let jdtls_tree = root.join("jdtls-tree");
        for folder in [
            "bin",
            "features",
            "plugins",
            "config_win",
            "config_linux",
            "config_mac",
        ] {
            fs::create_dir_all(jdtls_tree.join(folder)).unwrap();
        }
        fs::write(jdtls_tree.join("bin/jdtls.bat"), b"jdtls").unwrap();
        fs::write(jdtls_tree.join("config_win/settings"), b"win").unwrap();
        fs::write(jdtls_tree.join("config_linux/settings"), b"linux").unwrap();
        fs::write(jdtls_tree.join("config_mac/settings"), b"mac").unwrap();
        fs::write(
            jdtls_tree.join("plugins/org.eclipse.equinox.launcher_9.9.9.jar"),
            b"launcher",
        )
        .unwrap();
        fs::write(jdtls_tree.join("LICENSE.txt"), b"the license").unwrap();
        let feature = jdtls_tree.join("features/org.eclipse.equinox.executable_9.9.9.jar");
        let license_tree = root.join("license-tree");
        fs::create_dir_all(&license_tree).unwrap();
        fs::write(license_tree.join("epl-2.0.html"), b"the EPL text").unwrap();
        let mut command = Command::new(tar());
        command
            .args(["-a", "-c", "-f"])
            .arg(&feature)
            .arg("-C")
            .arg(&license_tree)
            .arg("epl-2.0.html");
        assert!(command.status().unwrap().success());

        let debug_tree = root.join("debug-tree");
        fs::create_dir_all(debug_tree.join("extension/server")).unwrap();
        fs::write(
            debug_tree.join("extension/server/com.microsoft.java.debug.plugin-9.9.9.jar"),
            b"plugin",
        )
        .unwrap();
        fs::write(debug_tree.join("extension/LICENSE.txt"), b"the MIT text").unwrap();
        fs::write(debug_tree.join("extension/other.txt"), b"not copied").unwrap();

        let jdtls = root.join(JDTLS_ARCHIVE);
        let debug = root.join(DEBUG_ARCHIVE);
        archive(&jdtls_tree, &jdtls, false);
        archive(&debug_tree, &debug, true);
        (jdtls, debug, root.join("tools"))
    }

    #[test]
    fn the_archives_are_unpacked_into_the_layout_the_search_expects() {
        let temp = TempDir::new("java-tools");
        let (jdtls, debug, tools) = fake_archives(temp.path());
        fs::create_dir_all(tools.join("download-jdtls")).unwrap();
        fs::create_dir_all(tools.join("download-java-debug")).unwrap();
        fs::copy(&jdtls, tools.join("download-jdtls").join(JDTLS_ARCHIVE)).unwrap();
        fs::copy(
            &debug,
            tools.join("download-java-debug").join(DEBUG_ARCHIVE),
        )
        .unwrap();
        let out = temp.path().join("out");

        let installed = install_java_tools(
            &out,
            &tools,
            &conpty::sha256_file(&jdtls).unwrap(),
            &conpty::sha256_file(&debug).unwrap(),
        )
        .unwrap();

        assert!(installed.jdtls.join("bin/jdtls.bat").is_file());
        assert!(installed.jdtls.join("config_win/settings").is_file());
        assert!(
            installed
                .jdtls
                .join("plugins/org.eclipse.equinox.launcher_9.9.9.jar")
                .is_file()
        );
        assert!(installed.jdtls.join("LICENSE.txt").is_file());
        assert!(installed.jdtls.join("epl-2.0.html").is_file());
        assert!(!installed.jdtls.join("config_linux").exists());
        assert!(!installed.jdtls.join("config_mac").exists());
        assert!(
            installed
                .debug
                .join("extension/server/com.microsoft.java.debug.plugin-9.9.9.jar")
                .is_file()
        );
        assert!(installed.debug.join("extension/LICENSE.txt").is_file());
        assert!(!installed.debug.join("extension/other.txt").exists());
        assert!(
            !out.join(format!(".unpack-jdtls-{}", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn a_wrong_hash_names_the_archive() {
        let temp = TempDir::new("java-tools-hash");
        let (jdtls, debug, tools) = fake_archives(temp.path());
        fs::create_dir_all(tools.join("download-jdtls")).unwrap();
        fs::create_dir_all(tools.join("download-java-debug")).unwrap();
        fs::copy(&jdtls, tools.join("download-jdtls").join(JDTLS_ARCHIVE)).unwrap();
        fs::copy(
            &debug,
            tools.join("download-java-debug").join(DEBUG_ARCHIVE),
        )
        .unwrap();

        let error = install_java_tools(
            &temp.path().join("out"),
            &tools,
            &"00".repeat(32),
            &conpty::sha256_file(&debug).unwrap(),
        )
        .err()
        .expect("a different archive is an error")
        .to_string();
        assert!(error.contains(JDTLS_ARCHIVE), "{error}");
        assert!(error.contains("SHA-256"), "{error}");
    }

    #[test]
    fn a_missing_archive_names_the_file_and_its_page() {
        let temp = TempDir::new("java-tools-missing");
        let tools = temp.path().join("tools");
        let error = install_java_tools(
            &temp.path().join("out"),
            &tools,
            &"00".repeat(32),
            &"00".repeat(32),
        )
        .err()
        .expect("a missing archive is an error")
        .to_string();
        assert!(error.contains(JDTLS_ARCHIVE), "{error}");
        assert!(error.contains(JDTLS_DOWNLOAD), "{error}");
    }

    #[test]
    fn third_party_groups_by_license_and_mentions_the_separate_servers() {
        let mut groups = BTreeMap::new();
        groups.insert("MIT".to_owned(), vec!["b 2".to_owned(), "a 1".to_owned()]);
        let text = render_third_party(&groups);
        assert!(text.contains("MIT (2 crates)"));
        let first = text.find("  a 1").unwrap();
        let second = text.find("  b 2").unwrap();
        assert!(first < second);
        assert!(text.contains("EPL-2.0"));
        assert!(text.contains("EPL-1.0"));
        assert!(text.contains("commons-io"));
        assert!(text.contains("RxJava"));
        assert!(text.contains("Reactive Streams"));
    }

    #[test]
    fn notice_carries_the_full_epl_1_0_text() {
        let temp = TempDir::new("epl10");
        let tools = JavaTools {
            jdtls: temp.path().join("jdtls"),
            debug: temp.path().join("debug"),
        };
        let text = java_notice(&tools);
        assert!(text.contains("Eclipse Public License - v 1.0"));
        assert!(text.contains("laws of the State of New York"));
        assert!(text.contains("extension\\EPL-1.0.txt"));
    }

    #[test]
    fn symbols_go_by_commit_next_to_the_distribution_not_into_it() {
        let temp = TempDir::new("symbols");
        let release = temp.path().join("target").join("release");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join(EXE), b"exe").unwrap();
        fs::write(release.join(PDB), b"pdb").unwrap();
        let out = temp.path().join("Arro");
        fs::create_dir_all(&out).unwrap();
        let symbols = default_symbols_dir(&out);
        assert_eq!(symbols, temp.path().join("Arro-symbols"));
        let saved = copy_symbols(&release, &symbols, "abc1234").unwrap();
        assert_eq!(saved, symbols.join("abc1234"));
        assert!(saved.join(EXE).is_file());
        assert!(saved.join(PDB).is_file());
        assert!(!out.join(PDB).exists());
    }
}
