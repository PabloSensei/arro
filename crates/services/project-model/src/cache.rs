use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ProjectModel;

/// Where project caches live by default: outside the project, the tree stays clean.
pub fn default_cache_root() -> PathBuf {
    base::app::cache_folder().join("projects")
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

/// Writes the model through a temporary file of its own: two syncs of one project, as a
/// reload while the first sync runs, never rename each other's file away.
pub(crate) fn store(file: &Path, model: &ProjectModel) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = file.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(directory)?;
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let temporary = file.with_extension(format!("{}-{unique}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(model)?)?;
    let renamed = fs::rename(&temporary, file);
    if renamed.is_err() {
        fs::remove_file(&temporary).ok();
    }
    renamed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncs_at_once_all_store_the_model() {
        let root = std::env::temp_dir().join(format!("arro-cache-race-{}", std::process::id()));
        let file = cache_file(&root, Path::new("C:/project"));
        let model = ProjectModel::default();
        let failed = std::thread::scope(|scope| {
            let writers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| (0..20).filter(|_| store(&file, &model).is_err()).count()))
                .collect();
            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(failed, 0);
        assert!(load(&file).is_some());
        let left: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(left.len(), 1, "temporary files are left: {left:?}");
        fs::remove_dir_all(&root).ok();
    }
}
