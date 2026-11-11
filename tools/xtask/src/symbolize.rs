//! Turns the `arro.exe+0xoffset` frames of a panic log into function names and source
//! lines. The `.pdb` of a release is kept outside the distribution, by commit, so a log
//! sent by a user is read with the symbols of exactly that build.

use std::fs;
use std::path::Path;

use crate::Result;

/// The module whose frames the log names; the others are printed as they are.
const MODULE: &str = "arro.exe";
/// `dist --out DIR` keeps the symbols in `DIR-symbols`; on a development machine the
/// same layout lives under `target/symbols`.
const DEFAULT_SYMBOLS: &str = "target/symbols";

pub fn run(root: &Path, log: &Path, symbols: Option<&Path>) -> Result {
    let text = fs::read_to_string(log)
        .map_err(|error| format!("cannot read {}: {error}", log.display()))?;
    let commit =
        commit_of(&text).ok_or_else(|| format!("{} has no commit: line", log.display()))?;
    let symbols = match symbols {
        Some(dir) => crate::dist::absolute(dir)?,
        None => root.join(DEFAULT_SYMBOLS),
    };
    let pdb = symbols.join(&commit).join("arro.pdb");
    if !pdb.is_file() {
        return Err(format!(
            "no symbols for commit {commit}: {} not found",
            pdb.display()
        )
        .into());
    }
    let text = symbolize_text(&text, &pdb, MODULE)?;
    print!("{text}");
    Ok(())
}

/// The commit of the build that wrote the log. Only a name that cannot leave the
/// symbols folder is accepted, because a log is untrusted input.
fn commit_of(text: &str) -> Option<String> {
    let commit = text
        .lines()
        .find_map(|line| line.strip_prefix("commit:"))?
        .trim();
    let safe = !commit.is_empty()
        && commit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    safe.then(|| commit.to_owned())
}

/// The whole log, with `function at file:line` appended to every frame of `module`.
fn symbolize_text(text: &str, pdb_path: &Path, module: &str) -> Result<String> {
    let file = fs::File::open(pdb_path)
        .map_err(|error| format!("cannot open {}: {error}", pdb_path.display()))?;
    let pdb = pdb_addr2line::pdb::PDB::open(file)
        .map_err(|error| format!("cannot read {}: {error}", pdb_path.display()))?;
    let data = pdb_addr2line::ContextPdbData::try_from_pdb(pdb)
        .map_err(|error| format!("cannot read {}: {error}", pdb_path.display()))?;
    let context = data
        .make_context()
        .map_err(|error| format!("cannot read {}: {error}", pdb_path.display()))?;

    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        result.push_str(line);
        if let Some(offset) = frame_offset(line, module) {
            let frames = context
                .find_frames(offset as u32)
                .map_err(|error| format!("cannot read {}: {error}", pdb_path.display()))?;
            match frames.and_then(|frames| frames.frames.into_iter().next()) {
                Some(frame) => {
                    let function = frame.function.as_deref().unwrap_or("??");
                    let location = match (frame.file.as_deref(), frame.line) {
                        (Some(file), Some(line)) => format!("{file}:{line}"),
                        (Some(file), None) => file.to_owned(),
                        _ => "??:?".to_owned(),
                    };
                    result.push_str(&format!("  {function} at {location}"));
                }
                None => result.push_str("  <no symbol>"),
            }
        }
        result.push('\n');
    }
    Ok(result)
}

/// The offset after `module+0x` in a line, if the line names that module.
fn frame_offset(line: &str, module: &str) -> Option<usize> {
    let marker = format!("{module}+0x");
    let start = line.find(&marker)? + marker.len();
    let end = line[start..]
        .find(|c: char| !c.is_ascii_hexdigit())
        .map_or(line.len(), |offset| start + offset);
    if end == start {
        return None;
    }
    usize::from_str_radix(&line[start..end], 16).ok()
}

#[cfg(test)]
mod tests {
    use super::{frame_offset, symbolize_text};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    #[test]
    fn a_test_binary_frame_resolves_to_its_function_and_file() {
        let exe = std::env::current_exe().unwrap();
        let pdb = exe.with_extension("pdb");
        assert!(pdb.is_file(), "no pdb next to {}", exe.display());
        // SAFETY: a null name asks for the module of this executable.
        let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
        assert_ne!(base, 0, "the test executable has no module handle");
        let offset =
            (a_test_binary_frame_resolves_to_its_function_and_file as *const () as usize) - base;
        let module = exe.file_name().unwrap().to_str().unwrap();
        let text = format!(
            "version: test\ncommit: test\nos: Windows\nbacktrace:\n   0: {module}+0x{offset:x}\n"
        );
        let symbolized = symbolize_text(&text, &pdb, module).unwrap();
        assert!(
            symbolized.contains("a_test_binary_frame_resolves_to_its_function_and_file"),
            "{symbolized}"
        );
        assert!(symbolized.contains("symbolize.rs"), "{symbolized}");
    }

    #[test]
    fn only_the_named_module_and_a_hex_offset_are_frames() {
        assert_eq!(
            frame_offset("   0: arro.exe+0x1a2b3c", "arro.exe"),
            Some(0x1a2b3c)
        );
        assert_eq!(
            frame_offset("   0: kernel32.dll+0x1a2b3c", "arro.exe"),
            None
        );
        assert_eq!(frame_offset("   0: arro.exe+0x", "arro.exe"), None);
    }
}
