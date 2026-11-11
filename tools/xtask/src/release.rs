//! The files of a GitHub release: one archive of a ready distribution folder and the
//! checksum file next to it, in the format `sha256sum` writes.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::{Result, dist};

const PLATFORM: &str = "windows-x86_64";

/// `release-assets --out DIR`: `DIR` is the folder `xtask dist` filled. The archive and
/// its checksum are written next to it.
pub fn run(out: &Path) -> Result {
    let out = dist::absolute(out)?;
    let archive = assets(&out)?;
    println!("{}", archive.display());
    Ok(())
}

/// Writes `arro-<version>-windows-x86_64.zip` and its `.sha256` next to `folder`.
pub(crate) fn assets(folder: &Path) -> Result<PathBuf> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()).into());
    }
    let parent = folder.parent().ok_or("the folder has no parent")?;
    let name = format!("arro-{}-{PLATFORM}.zip", env!("CARGO_PKG_VERSION"));
    let archive = parent.join(&name);
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let mut command = Command::new(dist::tar());
    command
        .args(["-a", "-c", "-f"])
        .arg(&archive)
        .arg("-C")
        .arg(folder)
        .arg(".");
    let status = command.status()?;
    if !status.success() {
        return Err(format!("tar failed: {status}").into());
    }

    let hash = sha256(&archive)?;
    let checksum = parent.join(format!("{name}.sha256"));
    let mut text = File::create(&checksum)?;
    writeln!(text, "{hash}  {name}")?;
    Ok(archive)
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-release-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_the_archive_and_a_sha256_line_next_to_the_folder() {
        let root = scratch("assets");
        let folder = root.join("arro");
        fs::create_dir_all(folder.join("helpers")).unwrap();
        fs::write(folder.join("ide.exe"), b"program").unwrap();
        fs::write(folder.join("helpers/one.jar"), b"jar").unwrap();

        let archive = assets(&folder).unwrap();
        let name = format!("arro-{}-{PLATFORM}.zip", env!("CARGO_PKG_VERSION"));
        assert_eq!(archive, root.join(&name));
        assert!(archive.is_file());
        let text = fs::read_to_string(root.join(format!("{name}.sha256"))).unwrap();
        let line = text.trim_end();
        let (hash, file) = line.split_once("  ").unwrap();
        assert_eq!(file, name);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, sha256(&archive).unwrap());

        // The archive holds the contents of the folder, not the folder itself.
        let unpacked = root.join("unpacked");
        fs::create_dir_all(&unpacked).unwrap();
        let status = Command::new(dist::tar())
            .args(["-x", "-f"])
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(unpacked.join("ide.exe")).unwrap(), b"program");
        assert_eq!(fs::read(unpacked.join("helpers/one.jar")).unwrap(), b"jar");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_folder_is_an_error() {
        let root = scratch("missing");
        assert!(assets(&root.join("nothing")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
