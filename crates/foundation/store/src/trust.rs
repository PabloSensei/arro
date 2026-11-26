//! Trust in a project folder. Agents and language servers run code of the project, so
//! whatever starts them asks here first. The dialog that asks the user lives in the UI.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const FILE: &str = "trusted-projects.txt";

/// Trusted project folders, one per line, next to the rest of the state. A line may name a
/// folder whose projects are all trusted, such as `~/workspace`. Paths compare without case,
/// as on Windows, and `..` is resolved before the comparison.
#[derive(Clone, Debug)]
pub struct TrustList {
    file: PathBuf,
}

/// The path without `.` and with every `..` that a name can take resolved. The path need not
/// exist: a project that is about to be created has no other answer.
fn normalized(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) | None => {}
                Some(_) => parts.push(component),
            },
            _ => parts.push(component),
        }
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part.as_os_str());
    }
    out
}

#[cfg(windows)]
fn same_component(first: Component<'_>, second: Component<'_>) -> bool {
    let (first, second) = (first.as_os_str(), second.as_os_str());
    first
        .to_string_lossy()
        .eq_ignore_ascii_case(&second.to_string_lossy())
}

#[cfg(not(windows))]
fn same_component(first: Component<'_>, second: Component<'_>) -> bool {
    first.as_os_str() == second.as_os_str()
}

/// `path` is `folder` or inside it.
fn is_same_or_inside(path: &Path, folder: &Path) -> bool {
    if folder.as_os_str().is_empty() {
        return false;
    }
    let mut path = path.components();
    for component in folder.components() {
        match path.next() {
            Some(found) if same_component(found, component) => {}
            _ => return false,
        }
    }
    true
}

impl TrustList {
    /// The list kept in `state_dir`, see `state_dir()`.
    pub fn new(state_dir: &Path) -> Self {
        Self {
            file: state_dir.join(FILE),
        }
    }

    /// Reads the file on every call: another window may have trusted the folder meanwhile.
    /// A project is trusted when its folder or one of its parents is on the list.
    pub fn is_trusted(&self, project: &Path) -> bool {
        let project = normalized(project);
        self.entries()
            .iter()
            .any(|line| is_same_or_inside(&project, &normalized(Path::new(line))))
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

    /// Forgets the trust of `project`: drops its line, and a line of a folder that holds it,
    /// so the project asks again the next time it opens.
    pub fn untrust(&self, project: &Path) -> io::Result<()> {
        let project = normalized(project);
        let text = fs::read_to_string(&self.file).unwrap_or_default();
        let kept: Vec<&str> = text
            .lines()
            .filter(|line| {
                let line = normalized(Path::new(line));
                !(is_same_or_inside(&project, &line) || is_same_or_inside(&line, &project))
            })
            .collect();
        let mut out = String::new();
        for line in kept {
            out.push_str(line);
            out.push('\n');
        }
        fs::write(&self.file, out)
    }

    fn entries(&self) -> Vec<String> {
        fs::read_to_string(&self.file)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
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

    #[test]
    fn a_trusted_folder_covers_the_projects_inside_it() {
        let base = test_support::scratch("trust-folder");
        let list = TrustList::new(&base.join("state"));
        let trusted = base.join("workspace");
        let inside = trusted.join("shop");
        let deeper = inside.join("src");
        let sibling = base.join("elsewhere").join("shop");
        let lookalike = base.join("workshop");

        list.trust(&trusted).unwrap();

        assert!(list.is_trusted(&trusted));
        assert!(list.is_trusted(&inside));
        assert!(list.is_trusted(&deeper));
        assert!(!list.is_trusted(&sibling));
        assert!(!list.is_trusted(&lookalike));
    }

    #[test]
    fn parent_components_are_resolved_before_the_comparison() {
        let base = test_support::scratch("trust-parent");
        let list = TrustList::new(&base.join("state"));
        let trusted = base.join("workspace");
        list.trust(&trusted).unwrap();

        let mut dotted = trusted.join("shop");
        dotted.push("..");
        dotted.push("shop");
        assert!(list.is_trusted(&dotted), "the path has a needless ..");

        #[cfg(windows)]
        {
            let upper = PathBuf::from(trusted.to_string_lossy().to_uppercase());
            assert!(list.is_trusted(&upper.join("SHOP")));
        }
    }

    #[test]
    fn untrusting_a_project_also_drops_the_folder_that_holds_it() {
        let base = test_support::scratch("trust-untrust");
        let list = TrustList::new(&base.join("state"));
        let trusted = base.join("workspace");
        let project = trusted.join("shop");
        list.trust(&trusted).unwrap();
        assert!(list.is_trusted(&project));

        list.untrust(&project).unwrap();

        assert!(!list.is_trusted(&project));
        assert!(!list.is_trusted(&trusted));
    }

    #[test]
    fn a_line_without_a_folder_trusts_nothing() {
        let base = test_support::scratch("trust-empty");
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join(FILE), ".\n..\n\n").unwrap();
        let list = TrustList::new(&state);

        assert!(!list.is_trusted(&base.join("shop")));
    }
}
