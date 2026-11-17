//! Which harnesses are installed. Only looks at files on PATH: starting a harness to ask for its
//! version would touch the user's home.

use std::env::split_paths;
use std::ffi::OsStr;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

use base::program::executable;
use terminal::command_line;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
    OpenCode,
}

impl Harness {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::OpenCode];

    pub fn stem(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Agent",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }
}

/// How to start a harness found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub harness: Harness,
    pub program: PathBuf,
    /// Comes before the arguments of a run: the script of an npm shim, for one.
    pub prefix: Vec<String>,
    /// A batch file has no process of its own and goes through `cmd.exe`.
    pub through_shell: bool,
}

impl Installed {
    pub fn command_line(&self, arguments: &[String]) -> String {
        let all: Vec<String> = self.prefix.iter().chain(arguments).cloned().collect();
        let line = command_line(&self.program, &all);
        if self.through_shell {
            return format!("cmd.exe /d /s /c \"{line}\"");
        }
        line
    }
}

fn find(path: &OsStr, file: &str) -> Option<PathBuf> {
    let candidates = split_paths(path).map(|directory| directory.join(file));
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Same entry point as the npm `codex` shim, minus the shell around it.
#[cfg(windows)]
fn codex_script(path: &OsStr, shim: &Path) -> Option<Installed> {
    let script = shim
        .parent()?
        .join("node_modules/@openai/codex/bin/codex.js");
    let node = find(path, &executable("node"))?;
    script.is_file().then(|| Installed {
        harness: Harness::Codex,
        program: node,
        prefix: vec![script.to_string_lossy().into_owned()],
        through_shell: false,
    })
}

fn locate(path: &OsStr, harness: Harness) -> Option<Installed> {
    let stem = harness.stem();
    if let Some(program) = find(path, &executable(stem)) {
        return Some(Installed {
            harness,
            program,
            prefix: Vec::new(),
            through_shell: false,
        });
    }
    // An npm install puts a batch file of the same name on PATH on Windows only; on
    // Unix the package manager links the entry point itself.
    #[cfg(windows)]
    {
        let shim = find(path, &format!("{stem}.cmd"))?;
        let direct = (harness == Harness::Codex)
            .then(|| codex_script(path, &shim))
            .flatten();
        Some(direct.unwrap_or(Installed {
            harness,
            program: shim,
            prefix: Vec::new(),
            through_shell: true,
        }))
    }
    #[cfg(unix)]
    {
        let _ = harness;
        None
    }
}

/// Every known harness on the given PATH value.
pub fn detect(path: &OsStr) -> Vec<Installed> {
    let found = Harness::ALL.iter().map(|harness| locate(path, *harness));
    found.flatten().collect()
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::fs;

    use super::*;

    struct Directory(test_support::TempDir);

    impl Directory {
        fn new(name: &str) -> Self {
            Self(test_support::TempDir::new(&format!("harness-{name}")))
        }

        #[cfg(windows)]
        fn touch(&self, relative: &str) {
            let file = self.0.join(relative);
            fs::create_dir_all(file.parent().expect("parent")).expect("directory");
            fs::write(file, "").expect("file");
        }
    }

    #[test]
    fn nothing_is_found_on_an_empty_path() {
        let directory = Directory::new("empty");

        assert!(detect(directory.0.as_os_str()).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn an_exe_wins_over_a_shim() {
        let directory = Directory::new("exe");
        directory.touch("claude.exe");
        directory.touch("claude.cmd");

        let found = detect(directory.0.as_os_str());

        assert_eq!(found.len(), 1);
        assert!(found[0].program.ends_with("claude.exe"));
        assert!(!found[0].through_shell);
    }

    #[cfg(windows)]
    #[test]
    fn the_codex_shim_resolves_to_node_and_script() {
        let directory = Directory::new("codex");
        directory.touch("codex.cmd");
        directory.touch("node.exe");
        directory.touch("node_modules/@openai/codex/bin/codex.js");

        let found = detect(directory.0.as_os_str());

        assert_eq!(found[0].harness, Harness::Codex);
        assert!(found[0].program.ends_with("node.exe"));
        assert!(found[0].prefix[0].ends_with("codex.js"));
    }

    #[cfg(windows)]
    #[test]
    fn a_plain_shim_runs_through_the_shell() {
        let directory = Directory::new("opencode");
        directory.touch("opencode.cmd");

        let found = detect(directory.0.as_os_str());
        let line = found[0].command_line(&["--flag".to_string()]);

        assert_eq!(found[0].harness, Harness::OpenCode);
        assert!(line.starts_with("cmd.exe /d /s /c \""));
        assert!(line.ends_with("--flag\""));
    }
}
