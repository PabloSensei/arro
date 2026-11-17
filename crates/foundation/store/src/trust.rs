//! Trust in a project folder. Agents and language servers run code of the project, so whatever
//! starts them asks here first. The dialog that asks the user lives in the UI.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const FILE: &str = "trusted-projects.txt";

/// Trusted project folders, one per line, next to the rest of the state. Paths compare without
/// case, as on Windows.
#[derive(Clone, Debug)]
pub struct TrustList {
    file: PathBuf,
}

fn same_folder(line: &str, project: &Path) -> bool {
    line.eq_ignore_ascii_case(&project.to_string_lossy())
}

impl TrustList {
    /// The list kept in `state_dir`, see `state_dir()`.
    pub fn new(state_dir: &Path) -> Self {
        Self {
            file: state_dir.join(FILE),
        }
    }

    /// Reads the file on every call: another window may have trusted the folder meanwhile.
    pub fn is_trusted(&self, project: &Path) -> bool {
        let text = fs::read_to_string(&self.file).unwrap_or_default();
        text.lines().any(|line| same_folder(line, project))
    }

    pub fn trust(&self, project: &Path) -> io::Result<()> {
        if self.is_trusted(project) {
            return Ok(());
        }
        let mut text = fs::read_to_string(&self.file).unwrap_or_default();
        text.push_str(&project.to_string_lossy());
        text.push('\n');
        if let Some(folder) = self.file.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(&self.file, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_remembered_per_folder() {
        let dir = test_support::scratch("trust");
        let list = TrustList::new(&dir);
        let project = Path::new(r"C:\work\Demo");
        assert!(!list.is_trusted(project));

        list.trust(project).unwrap();
        list.trust(project).unwrap();

        assert!(list.is_trusted(Path::new(r"c:\work\demo")));
        assert!(!list.is_trusted(Path::new(r"C:\work\Other")));
        let text = fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 1);
    }
}
