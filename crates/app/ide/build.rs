//! The version, commit, build date and channel the About window shows, and the icon and
//! version block of `arro.exe`. Without `git` the commit is `unknown`, and without the
//! Windows SDK the executable only lacks its icon; neither fails the build.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Runs this script again when the commit changes.
fn watch_head() {
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!(
            "cargo:rerun-if-changed={}",
            PathBuf::from(dir).join("HEAD").display()
        );
    }
    if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common = PathBuf::from(common);
        println!(
            "cargo:rerun-if-changed={}",
            common.join("packed-refs").display()
        );
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", common.join(branch).display());
        }
    }
}

/// `YYYY-MM-DD` in UTC, from the clock, without a date library.
fn today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let days = (seconds / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 to a calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    watch_head();
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let channel = std::env::var("ARRO_CHANNEL").unwrap_or_else(|_| {
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            "preview".into()
        } else {
            "dev".into()
        }
    });
    println!("cargo:rustc-env=IDE_VERSION={}", env!("CARGO_PKG_VERSION"));
    println!("cargo:rustc-env=IDE_COMMIT={commit}");
    println!("cargo:rustc-env=IDE_BUILD_DATE={}", today());
    println!("cargo:rustc-env=IDE_CHANNEL={channel}");
    embed_resources();
}

/// The icon GPUI loads for the window and the taskbar, and the strings that make the
/// file properties and the task manager call the program `Arro`. The resource compiler
/// comes with the Windows SDK; without it the build warns and links no resources.
fn embed_resources() {
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arro.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<u32> = version
        .split('.')
        .take(4)
        .filter_map(|part| part.parse().ok())
        .collect();
    parts.resize(4, 0);
    let file_version = parts
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let text = format!(
        "1 ICON \"{}\"\n\
         1 VERSIONINFO\n\
         \x20FILEVERSION {file_version}\n\
         \x20PRODUCTVERSION {file_version}\n\
         \x20FILEFLAGSMASK 0x3fL\n\
         \x20FILEFLAGS 0x0L\n\
         \x20FILEOS 0x40004L\n\
         \x20FILETYPE 0x1L\n\
         \x20FILESUBTYPE 0x0L\n\
         BEGIN\n\
         \x20   BLOCK \"StringFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       BLOCK \"040904b0\"\n\
         \x20       BEGIN\n\
         \x20           VALUE \"CompanyName\", \"\"\n\
         \x20           VALUE \"FileDescription\", \"Arro\"\n\
         \x20           VALUE \"FileVersion\", \"{version}\"\n\
         \x20           VALUE \"LegalCopyright\", \"Copyright 2026 Paweł Januszko\"\n\
         \x20           VALUE \"ProductName\", \"Arro\"\n\
         \x20           VALUE \"ProductVersion\", \"{version}\"\n\
         \x20       END\n\
         \x20   END\n\
         \x20   BLOCK \"VarFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       VALUE \"Translation\", 0x409, 1200\n\
         \x20   END\n\
         END\n",
        icon.to_string_lossy().replace('\\', "/"),
    );
    // `rc.exe` reads a resource file in the ANSI code page unless it starts with a
    // UTF-16 byte order mark, and the copyright has a `ł`.
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let rc = out_dir.join("arro.rc");
    if let Err(error) = fs::write(&rc, bytes) {
        println!("cargo:warning=the executable has no icon: cannot write {rc:?}: {error}");
        return;
    }
    if let Err(error) = embed_resource::compile(&rc, embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=the executable has no icon: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn dates_before_and_after_the_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
