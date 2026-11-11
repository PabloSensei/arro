use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ProjectModel;

/// Where project caches live by default: outside the project, the tree stays clean.
pub fn default_cache_root() -> PathBuf {
    base::app::folder("LOCALAPPDATA").join("projects")
}

/// The cached model of one project. FNV-1a keeps a directory name stable and short.
pub fn cache_file(cache_root: &Path, project_root: &Path) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in project_root.to_string_lossy().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    cache_root.join(format!("{hash:016x}")).join("model.json")
}

pub(crate) fn load(file: &Path) -> Option<ProjectModel> {
    serde_json::from_slice(&fs::read(file).ok()?).ok()
}

pub(crate) fn store(file: &Path, model: &ProjectModel) -> io::Result<()> {
    let directory = file.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(directory)?;
    let temporary = file.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(model)?)?;
    fs::rename(temporary, file)
}
