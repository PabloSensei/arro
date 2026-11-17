//! The pinned ConPTY files: fetching the NuGet package, checking SHA-256 and copying the two
//! x64 files into a distribution. `dist` and `fetch-conpty` share this module.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::Result;

pub(crate) const LOCK: &str = "crates/services/terminal/conpty.lock";
pub(crate) const VENDOR: &str = "crates/services/terminal/vendor";
const PACKAGE: &str = "Microsoft.Windows.Console.ConPTY";
const DOWNLOAD: &str = "https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY";
const CONPTY_DLL: &str = "runtimes/win-x64/native/conpty.dll";
const OPEN_CONSOLE: &str = "build/native/runtimes/x64/OpenConsole.exe";
const SHA256_HEX: usize = 64;

/// The values of `conpty.lock`. An empty string means that the value is not pinned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lock {
    pub(crate) version: String,
    pub(crate) package_sha256: String,
    pub(crate) conpty_dll_sha256: String,
    pub(crate) open_console_exe_sha256: String,
}

impl Lock {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    pub(crate) fn parse(text: &str) -> Self {
        let mut lock = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches(['"', '\'']).to_owned();
            match key.trim() {
                "version" => lock.version = value,
                "package_sha256" => lock.package_sha256 = hash(value),
                "conpty_dll_sha256" => lock.conpty_dll_sha256 = hash(value),
                "open_console_exe_sha256" => lock.open_console_exe_sha256 = hash(value),
                _ => {}
            }
        }
        lock
    }

    fn is_complete(&self) -> bool {
        !self.package_sha256.is_empty()
            && !self.conpty_dll_sha256.is_empty()
            && !self.open_console_exe_sha256.is_empty()
    }
}

/// Only 64 hex digits are a pinned hash; anything else counts as empty.
fn hash(value: String) -> String {
    let value = value.to_ascii_lowercase();
    if value.len() == SHA256_HEX && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        value
    } else {
        String::new()
    }
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(fs::read(path)?);
    Ok(format!("{:x}", digest.finalize()))
}

/// Downloads the package into `vendor/`, prints the hashes until they are pinned, and copies
/// the two x64 files to `vendor/x64` only after every value in the lock matches.
pub fn fetch(root: &Path) -> Result {
    let lock_path = root.join(LOCK);
    let lock = Lock::read(&lock_path)?;
    if lock.version.is_empty() {
        return Err(format!("{} has no version", lock_path.display()).into());
    }
    let vendor = root.join(VENDOR);
    fs::create_dir_all(&vendor)?;
    let package = vendor.join(format!("{PACKAGE}.{}.nupkg", lock.version));
    if package.is_file() {
        println!("fetch-conpty: reusing {}", package.display());
    } else {
        let url = format!("{DOWNLOAD}/{}", lock.version);
        println!("fetch-conpty: downloading {url}");
        download(&url, &package)?;
    }

    let package_hash = sha256_file(&package)?;
    if !lock.package_sha256.is_empty() && package_hash != lock.package_sha256 {
        let _ = fs::remove_file(&package);
        return Err(format!(
            "{}: SHA-256 {package_hash} does not match the pinned {}",
            package.display(),
            lock.package_sha256
        )
        .into());
    }

    let unpacked = vendor.join(format!(".unpacked-{}", std::process::id()));
    let _ = fs::remove_dir_all(&unpacked);
    let result = unpack(&package, &unpacked).and_then(|(dll, exe)| {
        let dll_hash = sha256_file(&dll)?;
        let exe_hash = sha256_file(&exe)?;
        if !lock.is_complete() {
            println!(
                "fetch-conpty: not installed, the hashes are not pinned in {}",
                lock_path.display()
            );
            println!("package_sha256 = {package_hash}");
            println!("conpty_dll_sha256 = {dll_hash}");
            println!("open_console_exe_sha256 = {exe_hash}");
            return Ok(());
        }
        if dll_hash != lock.conpty_dll_sha256 {
            return Err(format!(
                "conpty.dll SHA-256 {dll_hash} does not match the pinned {}",
                lock.conpty_dll_sha256
            )
            .into());
        }
        if exe_hash != lock.open_console_exe_sha256 {
            return Err(format!(
                "OpenConsole.exe SHA-256 {exe_hash} does not match the pinned {}",
                lock.open_console_exe_sha256
            )
            .into());
        }
        let target = vendor.join("x64");
        fs::create_dir_all(&target)?;
        fs::copy(&dll, target.join("conpty.dll"))?;
        fs::copy(&exe, target.join("OpenConsole.exe"))?;
        println!(
            "fetch-conpty: installed conpty.dll and OpenConsole.exe into {}",
            target.display()
        );
        Ok(())
    });
    let _ = fs::remove_dir_all(&unpacked);
    result
}

/// Downloads through the `curl.exe` of Windows 11; no HTTP client dependency is needed.
fn download(url: &str, path: &Path) -> Result {
    let curl = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("curl.exe"))
        .filter(|tool| tool.is_file())
        .unwrap_or_else(|| PathBuf::from("curl"));
    let status = Command::new(curl)
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(path)
        .arg(url)
        .status()?;
    if !status.success() {
        let _ = fs::remove_file(path);
        return Err(format!("downloading {url} failed: {status}").into());
    }
    Ok(())
}

/// Extracts the archive and returns the two x64 files. The known paths come first; a package
/// with another layout is searched by file name, with the x64 copies preferred.
fn unpack(archive: &Path, directory: &Path) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(directory)?;
    let status = Command::new(tar())
        .args(["-x", "-f"])
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .status()?;
    if !status.success() {
        return Err(format!("extracting {} failed: {status}", archive.display()).into());
    }
    let dll =
        locate(directory, CONPTY_DLL, "conpty.dll").ok_or("the package has no x64 conpty.dll")?;
    let exe = locate(directory, OPEN_CONSOLE, "OpenConsole.exe")
        .ok_or("the package has no x64 OpenConsole.exe")?;
    Ok((dll, exe))
}

fn locate(root: &Path, preferred: &str, name: &str) -> Option<PathBuf> {
    let exact = root.join(preferred);
    if exact.is_file() {
        return Some(exact);
    }
    let mut found = Vec::new();
    collect_files(root, name, &mut found);
    found.sort();
    found
        .iter()
        .find(|path| path.to_string_lossy().to_ascii_lowercase().contains("x64"))
        .or_else(|| found.first())
        .cloned()
}

fn collect_files(directory: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, name, found);
        } else if path
            .file_name()
            .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
        {
            found.push(path);
        }
    }
}

/// The Windows `tar.exe` reads and writes the zip format; `dist` uses it for the archive too.
fn tar() -> PathBuf {
    if let Some(root) = env::var_os("SystemRoot") {
        let tool = PathBuf::from(root).join("System32").join("tar.exe");
        if tool.is_file() {
            return tool;
        }
    }
    PathBuf::from("tar")
}

/// Copies the vendored files next to `arro.exe` when both are present and pinned. A missing or
/// unpinned pair is not an error: the release then uses the system ConPTY.
pub(crate) fn install(root: &Path, out: &Path) -> Result<bool> {
    let lock = Lock::read(&root.join(LOCK))?;
    let vendor = root.join(VENDOR).join("x64");
    let dll = vendor.join("conpty.dll");
    let exe = vendor.join("OpenConsole.exe");
    if !dll.is_file() || !exe.is_file() {
        println!(
            "dist: {} has no conpty.dll and OpenConsole.exe, the terminal will use the system ConPTY",
            vendor.display()
        );
        return Ok(false);
    }
    let dll_hash = sha256_file(&dll)?;
    let exe_hash = sha256_file(&exe)?;
    if lock.conpty_dll_sha256.is_empty() || lock.open_console_exe_sha256.is_empty() {
        println!(
            "dist: the ConPTY files are not pinned in {}, the terminal will use the system ConPTY",
            root.join(LOCK).display()
        );
        return Ok(false);
    }
    if dll_hash != lock.conpty_dll_sha256 || exe_hash != lock.open_console_exe_sha256 {
        println!(
            "dist: {} does not match {}, the terminal will use the system ConPTY",
            vendor.display(),
            root.join(LOCK).display()
        );
        return Ok(false);
    }
    fs::copy(&dll, out.join("conpty.dll"))?;
    fs::copy(&exe, out.join("OpenConsole.exe"))?;
    println!("dist: conpty.dll and OpenConsole.exe");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use test_support::TempDir;

    fn write_tree(root: &Path, dll: &[u8], exe: &[u8]) {
        let dll_path = root.join(CONPTY_DLL);
        let exe_path = root.join(OPEN_CONSOLE);
        fs::create_dir_all(dll_path.parent().unwrap()).unwrap();
        fs::create_dir_all(exe_path.parent().unwrap()).unwrap();
        fs::write(dll_path, dll).unwrap();
        fs::write(exe_path, exe).unwrap();
    }

    fn zip_tree(root: &Path, tree: &Path) -> PathBuf {
        let archive = root.join("package.nupkg");
        let status = Command::new(tar())
            .args(["-a", "-c", "-f"])
            .arg(&archive)
            .arg("-C")
            .arg(tree)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    fn write_lock(root: &Path, lock: &Lock) {
        let path = root.join(LOCK);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "package = {PACKAGE}\nversion = 1.0\npackage_sha256 = {}\nconpty_dll_sha256 = {}\nopen_console_exe_sha256 = {}\n",
                lock.package_sha256, lock.conpty_dll_sha256, lock.open_console_exe_sha256
            ),
        )
        .unwrap();
    }

    #[test]
    fn the_lock_reads_the_version_and_hashes() {
        let hash = "ab".repeat(32);
        let lock = Lock::parse(&format!(
            "# pinned\nversion = 1.2.3\npackage_sha256 = {hash}\nconpty_dll_sha256 =\n"
        ));
        assert_eq!(lock.version, "1.2.3");
        assert_eq!(lock.package_sha256, hash);
        assert!(lock.conpty_dll_sha256.is_empty());
        assert!(!lock.is_complete());
    }

    #[test]
    fn a_test_zip_with_the_nuget_layout_is_unpacked() {
        let temp = TempDir::new("unpack");
        let tree = temp.path().join("tree");
        write_tree(&tree, b"dll bytes", b"exe bytes");
        let archive = zip_tree(temp.path(), &tree);
        let unpacked = temp.path().join("unpacked");
        let (dll, exe) = unpack(&archive, &unpacked).unwrap();
        assert_eq!(fs::read(dll).unwrap(), b"dll bytes");
        assert_eq!(fs::read(exe).unwrap(), b"exe bytes");
    }

    #[test]
    fn a_package_with_another_layout_is_searched_by_name() {
        let temp = TempDir::new("search");
        let tree = temp.path().join("tree");
        fs::create_dir_all(tree.join("other/x64/native")).unwrap();
        fs::write(tree.join("other/x64/native/OpenConsole.exe"), b"exe").unwrap();
        fs::create_dir_all(tree.join("other/win-x64")).unwrap();
        fs::write(tree.join("other/win-x64/conpty.dll"), b"dll").unwrap();
        let archive = zip_tree(temp.path(), &tree);
        let (dll, exe) = unpack(&archive, &temp.path().join("out")).unwrap();
        assert_eq!(fs::read(dll).unwrap(), b"dll");
        assert_eq!(fs::read(exe).unwrap(), b"exe");
    }

    #[test]
    fn fetch_installs_a_pinned_package_already_in_vendor() {
        let temp = TempDir::new("fetch");
        let root = temp.path();
        let tree = root.join("tree");
        write_tree(&tree, b"dll bytes", b"exe bytes");
        let vendor = root.join(VENDOR);
        fs::create_dir_all(&vendor).unwrap();
        let package = vendor.join(format!("{PACKAGE}.1.0.nupkg"));
        fs::copy(zip_tree(root, &tree), &package).unwrap();
        let lock = Lock {
            version: "1.0".into(),
            package_sha256: sha256_file(&package).unwrap(),
            conpty_dll_sha256: sha256_file(&tree.join(CONPTY_DLL)).unwrap(),
            open_console_exe_sha256: sha256_file(&tree.join(OPEN_CONSOLE)).unwrap(),
        };
        write_lock(root, &lock);
        fetch(root).unwrap();
        assert_eq!(
            fs::read(vendor.join("x64/conpty.dll")).unwrap(),
            b"dll bytes"
        );
        assert_eq!(
            fs::read(vendor.join("x64/OpenConsole.exe")).unwrap(),
            b"exe bytes"
        );
    }

    #[test]
    fn fetch_with_an_empty_lock_prints_hashes_and_installs_nothing() {
        let temp = TempDir::new("fetch-empty");
        let root = temp.path();
        let tree = root.join("tree");
        write_tree(&tree, b"dll bytes", b"exe bytes");
        let vendor = root.join(VENDOR);
        fs::create_dir_all(&vendor).unwrap();
        fs::copy(
            zip_tree(root, &tree),
            vendor.join(format!("{PACKAGE}.1.0.nupkg")),
        )
        .unwrap();
        write_lock(root, &Lock::default());
        fetch(root).unwrap();
        assert!(!vendor.join("x64/conpty.dll").exists());
        assert!(!vendor.join("x64/OpenConsole.exe").exists());
    }

    #[test]
    fn install_copies_only_matching_files() {
        let temp = TempDir::new("install");
        let root = temp.path();
        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        let vendor = root.join(VENDOR).join("x64");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("conpty.dll"), b"dll").unwrap();
        fs::write(vendor.join("OpenConsole.exe"), b"exe").unwrap();
        let lock = Lock {
            version: "1.0".into(),
            package_sha256: "00".repeat(32),
            conpty_dll_sha256: sha256_file(&vendor.join("conpty.dll")).unwrap(),
            open_console_exe_sha256: sha256_file(&vendor.join("OpenConsole.exe")).unwrap(),
        };
        write_lock(root, &lock);
        assert!(install(root, &out).unwrap());
        assert_eq!(fs::read(out.join("conpty.dll")).unwrap(), b"dll");
        assert_eq!(fs::read(out.join("OpenConsole.exe")).unwrap(), b"exe");
    }

    #[test]
    fn install_skips_missing_and_unpinned_files() {
        let temp = TempDir::new("skip");
        let root = temp.path();
        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        write_lock(root, &Lock::default());
        assert!(!install(root, &out).unwrap());
        assert!(!out.join("conpty.dll").exists());

        let vendor = root.join(VENDOR).join("x64");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("conpty.dll"), b"dll").unwrap();
        fs::write(vendor.join("OpenConsole.exe"), b"exe").unwrap();
        assert!(!install(root, &out).unwrap());
        assert!(!out.join("conpty.dll").exists());
    }

    #[test]
    fn install_skips_a_pinned_mismatch() {
        let temp = TempDir::new("mismatch");
        let root = temp.path();
        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        let vendor = root.join(VENDOR).join("x64");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("conpty.dll"), b"dll").unwrap();
        fs::write(vendor.join("OpenConsole.exe"), b"exe").unwrap();
        write_lock(
            root,
            &Lock {
                version: "1.0".into(),
                package_sha256: "00".repeat(32),
                conpty_dll_sha256: "1".repeat(32),
                open_console_exe_sha256: "2".repeat(32),
            },
        );
        assert!(!install(root, &out).unwrap());
        assert!(!out.join("conpty.dll").exists());
    }
}
