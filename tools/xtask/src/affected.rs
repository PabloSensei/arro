//! `affected`: the workspace crates a change touches. A file belongs to the crate with
//! the longest directory prefix, and a crate is affected when it is changed itself or
//! when any workspace crate depends on it, directly or transitively, tests included.
//! A file outside the crates, such as a script or a query, touches the crates whose
//! sources name it, and everything when none does.
//! The command prints the crates as `-p` arguments or runs their checks.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::{Result, cargo};

const XTASK: &str = "xtask";

/// Directories outside the crates that no check compiles.
const QUIET_DIRS: &[&str] = &["team", "docs", ".github"];

/// Root files outside the crates that change how every crate builds.
const ROOT_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "deny.toml",
];

/// Root directories outside the crates that change how every crate builds. `vendor` holds
/// patched dependencies that reach the crates through the manifest, not through a mention.
const ROOT_DIRS: &[&str] = &[".cargo", "vendor"];

/// The checks `--run` can start.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Test,
    Clippy,
    Both,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "test" => Ok(Self::Test),
            "clippy" => Ok(Self::Clippy),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown affected run: {other}; test, clippy or both").into()),
        }
    }

    fn clippy(self) -> bool {
        matches!(self, Self::Clippy | Self::Both)
    }

    fn test(self) -> bool {
        matches!(self, Self::Test | Self::Both)
    }
}

/// Prints the crates changed by `base..HEAD` and the working tree, and runs their checks.
pub fn run(root: &Path, base: &str, run: Option<Mode>, list: bool) -> Result {
    let workspace = Workspace::load(root)?;
    let changed = changed_files(root, base)?;
    let affected = workspace.affected(&changed);
    if affected.is_empty() {
        println!("nothing is affected");
        return Ok(());
    }
    if list {
        for name in &affected {
            println!("{name}");
        }
    } else {
        println!("{}", packages(&affected));
    }
    match run {
        Some(mode) => run_checks(root, mode, &affected),
        None => Ok(()),
    }
}

fn packages(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("-p {name}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_checks(root: &Path, mode: Mode, names: &[String]) -> Result {
    let mut steps: Vec<Vec<String>> = Vec::new();
    if mode.clippy() {
        let mut args = command("clippy", names);
        args.extend(["--all-targets", "--", "-D", "warnings"].map(str::to_owned));
        steps.push(args);
    }
    if mode.test() {
        // Windows cannot relink the running xtask.exe, so xtask is tested alone, as in ci.rs.
        let others: Vec<String> = names
            .iter()
            .filter(|name| *name != XTASK)
            .cloned()
            .collect();
        if !others.is_empty() {
            steps.push(command("test", &others));
        }
        if names.iter().any(|name| name == XTASK) {
            steps.push(command("test", &[XTASK.to_owned()]));
        }
    }
    for step in steps {
        println!("==> cargo {}", step.join(" "));
        // Tests press every button; links they open are recorded, not sent to a browser.
        let status = Command::new(cargo())
            .args(&step)
            .current_dir(root)
            .env("ARRO_NO_BROWSER", "1")
            .status()?;
        if !status.success() {
            return Err(format!("cargo {} failed", step.join(" ")).into());
        }
    }
    Ok(())
}

fn command(subcommand: &str, names: &[String]) -> Vec<String> {
    let mut args = vec![subcommand.to_owned()];
    for name in names {
        args.push("-p".to_owned());
        args.push(name.clone());
    }
    args
}

/// The commit range of `base..HEAD` and the uncommitted paths, sorted.
fn changed_files(root: &Path, base: &str) -> Result<Vec<String>> {
    let merge_base = git(root, &["merge-base", base, "HEAD"])?;
    let merge_base = merge_base.trim();
    if merge_base.is_empty() {
        return Err(format!("git found no common ancestor of {base} and HEAD").into());
    }
    let mut files = BTreeSet::new();
    for line in git(root, &["diff", "--name-only", merge_base, "HEAD"])?.lines() {
        let path = line.trim_end_matches('\r').trim();
        if !path.is_empty() {
            files.insert(path.replace('\\', "/"));
        }
    }
    for line in git(root, &["status", "--porcelain"])?.lines() {
        if let Some(path) = status_path(line) {
            files.insert(path);
        }
    }
    Ok(files.into_iter().collect())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), stderr.trim()).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The path of one `git status --porcelain` line, the new name of a rename.
fn status_path(line: &str) -> Option<String> {
    let path = line.trim_end_matches('\r').get(3..)?;
    let path = path.rsplit(" -> ").next().unwrap_or(path).trim();
    let path = path
        .strip_prefix('"')
        .and_then(|it| it.strip_suffix('"'))
        .unwrap_or(path);
    (!path.is_empty()).then(|| path.replace('\\', "/"))
}

struct Package {
    name: String,
    /// Workspace-relative directory with `/` separators.
    directory: String,
}

/// What a changed file belongs to.
enum Target {
    Crates(Vec<String>),
    Everything,
    Nothing,
}

struct Workspace {
    /// Sorted by name, so every output keeps one order.
    packages: Vec<Package>,
    /// A crate name and the workspace crates that depend on it.
    dependents: HashMap<String, Vec<String>>,
    /// A crate name and the texts of its own `*.rs` and `Cargo.toml` files, read once
    /// so a file outside the crates is matched against them without more disk work.
    sources: HashMap<String, Vec<String>>,
}

impl Workspace {
    fn load(root: &Path) -> Result<Self> {
        let output = Command::new(cargo())
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(root)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("cargo metadata failed: {}", stderr.trim()).into());
        }
        Self::parse(&serde_json::from_slice(&output.stdout)?)
    }

    fn parse(metadata: &Value) -> Result<Self> {
        let root = metadata["workspace_root"]
            .as_str()
            .ok_or("metadata has no workspace_root")?;
        let root = Path::new(root);
        let members: HashSet<&str> = array(&metadata["workspace_members"])
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let packages: Vec<&Value> = array(&metadata["packages"])
            .iter()
            .filter(|package| members.contains(string(&package["id"]).as_str()))
            .collect();
        let names: HashSet<&str> = packages
            .iter()
            .filter_map(|package| package["name"].as_str())
            .collect();

        let mut entries = Vec::new();
        let mut edges = Vec::new();
        for package in packages {
            let name = string(&package["name"]);
            let manifest = Path::new(package["manifest_path"].as_str().unwrap_or_default());
            let directory = manifest.parent().ok_or("a manifest has no directory")?;
            entries.push(Package {
                name: name.clone(),
                directory: directory
                    .strip_prefix(root)
                    .unwrap_or(directory)
                    .to_string_lossy()
                    .replace('\\', "/"),
            });
            for dependency in array(&package["dependencies"]) {
                // A registry package cannot be a workspace member; path ones can.
                if !dependency["source"].is_null() {
                    continue;
                }
                let dependency = string(&dependency["name"]);
                if names.contains(dependency.as_str()) {
                    edges.push((name.clone(), dependency));
                }
            }
        }
        Ok(Self::new(entries, edges).with_sources(root))
    }

    fn new(packages: Vec<Package>, edges: Vec<(String, String)>) -> Self {
        let mut packages = packages;
        packages.sort_by(|left, right| left.name.cmp(&right.name));
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
        for (dependent, dependency) in edges {
            dependents.entry(dependency).or_default().push(dependent);
        }
        for dependents in dependents.values_mut() {
            dependents.sort();
            dependents.dedup();
        }
        Self {
            packages,
            dependents,
            sources: HashMap::new(),
        }
    }

    /// Reads the sources of every crate once, so one command run touches the disk once
    /// whatever the number of changed files.
    fn with_sources(mut self, root: &Path) -> Self {
        let mut sources = HashMap::new();
        for package in &self.packages {
            let mut texts = Vec::new();
            collect_sources(&root.join(&package.directory), &mut texts);
            sources.insert(package.name.clone(), texts);
        }
        self.sources = sources;
        self
    }

    /// The names of the crates changed by `changed`, sorted, with their dependents.
    fn affected(&self, changed: &[String]) -> Vec<String> {
        let mut seeds = HashSet::new();
        for path in changed {
            match self.target(path) {
                Target::Everything => {
                    return self
                        .packages
                        .iter()
                        .map(|package| package.name.clone())
                        .collect();
                }
                Target::Crates(names) => {
                    seeds.extend(names);
                }
                Target::Nothing => {}
            }
        }
        let mut affected = seeds;
        let mut pending: Vec<String> = affected.iter().cloned().collect();
        while let Some(next) = pending.pop() {
            let edges = self.dependents.get(&next).map(Vec::as_slice);
            for dependent in edges.unwrap_or_default() {
                if affected.insert(dependent.clone()) {
                    pending.push(dependent.clone());
                }
            }
        }
        self.packages
            .iter()
            .filter(|package| affected.contains(&package.name))
            .map(|package| package.name.clone())
            .collect()
    }

    /// The crates whose directory is the longest whole-segment prefix of `path`, or the
    /// crates that name a file outside the crates in their sources.
    fn target(&self, path: &str) -> Target {
        if let Some(name) = self.package_of(path) {
            return Target::Crates(vec![name.to_owned()]);
        }
        let first = path.split('/').next().unwrap_or_default();
        let quiet = QUIET_DIRS.contains(&first) || path.ends_with(".md");
        if quiet {
            return Target::Nothing;
        }
        // Root manifests, the toolchain and `.cargo` change how every crate builds.
        if ROOT_FILES.contains(&path) || ROOT_DIRS.iter().any(|dir| within(dir, path)) {
            return Target::Everything;
        }
        // A script, a query or an asset outside the crates is read by the crates that
        // name it: `include_str!("../../../../queries/xml/highlights.scm")` names the
        // file, `join("../../queries/xml")` names its directory.
        let readers = self.readers(path);
        if readers.is_empty() {
            // Nothing in the workspace names the file, so it may change any build.
            return Target::Everything;
        }
        Target::Crates(readers)
    }

    /// The crates whose sources name the file `path`, by the whole path or its directory.
    fn readers(&self, path: &str) -> Vec<String> {
        let mut needles = vec![path.to_owned()];
        if let Some(directory) = path.rsplit_once('/').map(|(directory, _)| directory) {
            needles.push(format!("{directory}/"));
            needles.push(format!("{directory}\""));
        }
        self.packages
            .iter()
            .filter(|package| {
                self.sources.get(&package.name).is_some_and(|texts| {
                    texts
                        .iter()
                        .any(|text| needles.iter().any(|needle| text.contains(needle)))
                })
            })
            .map(|package| package.name.clone())
            .collect()
    }

    fn package_of(&self, path: &str) -> Option<&str> {
        let mut best: Option<&Package> = None;
        for package in &self.packages {
            if !within(&package.directory, path) {
                continue;
            }
            let longer = best.is_none_or(|it| it.directory.len() < package.directory.len());
            if longer {
                best = Some(package);
            }
        }
        best.map(|package| package.name.as_str())
    }
}

/// Whether workspace-relative `path` stands in `directory`, by whole segments.
fn within(directory: &str, path: &str) -> bool {
    path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a file inside a crate can name a path outside it: a Rust source or the
/// manifest of the crate itself.
fn reads_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
        || path.file_name().is_some_and(|name| name == "Cargo.toml")
}

/// Collects the texts of the source files under `directory`, subdirectories included.
/// An unreadable file is skipped, which leaves the fallback of the caller in charge.
fn collect_sources(directory: &Path, texts: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if entry.file_name() != "target" {
                collect_sources(&path, texts);
            }
        } else if reads_source(&path)
            && let Ok(text) = fs::read_to_string(&path)
        {
            texts.push(text);
        }
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Package, Workspace, status_path};

    /// A small graph: `(name, directory)` and `(dependent, dependency)`.
    fn workspace(crates: &[(&str, &str)], edges: &[(&str, &str)]) -> Workspace {
        let packages = crates
            .iter()
            .map(|(name, directory)| Package {
                name: (*name).to_owned(),
                directory: (*directory).to_owned(),
            })
            .collect();
        let edges = edges
            .iter()
            .map(|(dependent, dependency)| ((*dependent).to_owned(), (*dependency).to_owned()))
            .collect();
        Workspace::new(packages, edges)
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    /// A graph whose crates have the given source texts.
    fn sources(graph: Workspace, texts: &[(&str, &[&str])]) -> Workspace {
        let mut graph = graph;
        graph.sources = texts
            .iter()
            .map(|(name, texts)| {
                let texts = texts.iter().map(|text| (*text).to_owned()).collect();
                ((*name).to_owned(), texts)
            })
            .collect();
        graph
    }

    #[test]
    fn a_path_belongs_to_the_crate_with_the_longest_directory() {
        let graph = workspace(
            &[
                ("outer", "crates/outer"),
                ("outermost", "crates/outermost"),
                ("inner", "crates/outer/inner"),
            ],
            &[],
        );
        assert_eq!(
            graph.affected(&paths(&["crates/outer/src/lib.rs"])),
            ["outer"]
        );
        assert_eq!(
            graph.affected(&paths(&["crates/outer/Cargo.toml"])),
            ["outer"]
        );
        assert_eq!(
            graph.affected(&paths(&["crates/outer/inner/src/lib.rs"])),
            ["inner"]
        );
        assert_eq!(
            graph.affected(&paths(&["crates/outermost/src/lib.rs"])),
            ["outermost"]
        );
    }

    #[test]
    fn documentation_github_and_the_board_touch_no_crate() {
        let graph = workspace(&[("one", "crates/one"), ("two", "crates/two")], &[]);
        let quiet = paths(&[
            "README.md",
            "docs/ARCHITECTURE.md",
            "docs/notes.txt",
            ".github/workflows/ci.yml",
            "team/BOARD.md",
        ]);
        assert_eq!(graph.affected(&quiet), [] as [&str; 0]);
        // A markdown file inside a crate still belongs to that crate.
        assert_eq!(graph.affected(&paths(&["crates/one/README.md"])), ["one"]);
    }

    #[test]
    fn a_root_manifest_affects_every_crate() {
        let graph = workspace(&[("two", "crates/two"), ("one", "crates/one")], &[]);
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "deny.toml",
            ".cargo/config.toml",
        ] {
            assert_eq!(graph.affected(&paths(&[path])), ["one", "two"], "{path}");
        }
    }

    #[test]
    fn a_change_reaches_the_crates_that_depend_on_it() {
        // `app` depends on `view`, `view` on `text`; `tests` uses `text` in tests only.
        let graph = workspace(
            &[
                ("text", "crates/text"),
                ("view", "crates/view"),
                ("app", "crates/app"),
                ("tests", "crates/tests"),
                ("other", "crates/other"),
            ],
            &[("view", "text"), ("app", "view"), ("tests", "text")],
        );
        assert_eq!(
            graph.affected(&paths(&["crates/text/src/lib.rs"])),
            ["app", "tests", "text", "view"]
        );
        assert_eq!(
            graph.affected(&paths(&["crates/view/src/lib.rs"])),
            ["app", "view"]
        );
        assert_eq!(
            graph.affected(&paths(&["crates/other/src/lib.rs"])),
            ["other"]
        );
    }

    #[test]
    fn the_order_is_by_name_whatever_the_input() {
        let graph = workspace(
            &[
                ("zeta", "crates/zeta"),
                ("alpha", "crates/alpha"),
                ("mid", "crates/mid"),
            ],
            &[("zeta", "alpha")],
        );
        assert_eq!(
            graph.affected(&paths(&["crates/alpha/src/lib.rs"])),
            ["alpha", "zeta"]
        );
        assert_eq!(
            graph.affected(&paths(&["Cargo.toml"])),
            ["alpha", "mid", "zeta"]
        );
    }

    #[test]
    fn a_status_line_gives_the_new_name_of_a_rename() {
        assert_eq!(
            status_path(" M crates/a/src/lib.rs"),
            Some("crates/a/src/lib.rs".to_owned())
        );
        assert_eq!(
            status_path("?? docs/new.md"),
            Some("docs/new.md".to_owned())
        );
        assert_eq!(
            status_path("R  crates/old/src/lib.rs -> crates/new/src/lib.rs"),
            Some("crates/new/src/lib.rs".to_owned())
        );
    }

    #[test]
    fn a_file_outside_the_crates_belongs_to_the_crates_that_name_it() {
        let graph = sources(
            workspace(
                &[
                    ("reader", "crates/reader"),
                    ("other", "crates/other"),
                    ("app", "crates/app"),
                ],
                &[("app", "reader")],
            ),
            &[
                (
                    "reader",
                    &[
                        r#"const HIGHLIGHTS: &str = include_str!("../../../../queries/xml/highlights.scm");"#,
                        r#"let script = root.join("../../../tools/smoke/release.script");"#,
                    ],
                ),
                ("other", &["fn run() {}"]),
            ],
        );
        // The source spells both paths from its own directory, with `../` in front.
        assert_eq!(
            graph.affected(&paths(&["queries/xml/highlights.scm"])),
            ["app", "reader"]
        );
        assert_eq!(
            graph.affected(&paths(&["tools/smoke/release.script"])),
            ["app", "reader"]
        );
        // Naming one file of a directory names the directory, so its other files count.
        assert_eq!(
            graph.affected(&paths(&["queries/xml/folds.scm"])),
            ["app", "reader"]
        );
        // Nobody names the file, so it may change any crate.
        assert_eq!(
            graph.affected(&paths(&["assets/icons/logo.svg"])),
            ["app", "other", "reader"]
        );
    }

    #[test]
    fn a_directory_mention_covers_every_file_below_it() {
        let graph = sources(
            workspace(
                &[
                    ("quoted", "crates/quoted"),
                    ("slashed", "crates/slashed"),
                    ("other", "crates/other"),
                ],
                &[],
            ),
            &[
                ("quoted", &[r#"root.join("../../queries/xml")"#]),
                ("slashed", &[r#"root.join("../../queries/yaml/")"#]),
                ("other", &["fn run() {}"]),
            ],
        );
        assert_eq!(
            graph.affected(&paths(&["queries/xml/highlights.scm"])),
            ["quoted"]
        );
        assert_eq!(
            graph.affected(&paths(&["queries/yaml/folds.scm"])),
            ["slashed"]
        );
        assert_eq!(
            graph.affected(&paths(&["queries/java/highlights.scm"])),
            ["other", "quoted", "slashed"]
        );
    }

    #[test]
    fn a_root_manifest_wins_over_a_mention_in_a_source() {
        // The crate names the root manifest, yet a root manifest changes every build.
        let graph = sources(
            workspace(&[("one", "crates/one"), ("two", "crates/two")], &[]),
            &[("one", &["// the root Cargo.toml"])],
        );
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "deny.toml",
            ".cargo/config.toml",
        ] {
            assert_eq!(graph.affected(&paths(&[path])), ["one", "two"], "{path}");
        }
    }

    #[test]
    fn the_real_graph_reads_the_release_script_only_in_panels_and_the_app() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let graph = Workspace::load(&root).unwrap();
        let affected = graph.affected(&paths(&["tools/smoke/release.script"]));
        for name in ["panels", "ide"] {
            assert!(
                affected.iter().any(|it| it == name),
                "{name} does not read the release script: {affected:?}"
            );
        }
        assert!(
            !affected.iter().any(|it| it == "text-core"),
            "the release script affected text-core: {affected:?}"
        );
    }

    #[test]
    fn the_real_graph_reads_the_xml_queries_only_in_file_syntax() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let graph = Workspace::load(&root).unwrap();
        let affected = graph.affected(&paths(&["queries/xml/highlights.scm"]));
        // `panels` and `ide` depend on `file-syntax`, so they follow it.
        for name in ["file-syntax", "panels", "ide"] {
            assert!(
                affected.iter().any(|it| it == name),
                "{name} does not read the xml queries: {affected:?}"
            );
        }
        assert!(
            !affected.iter().any(|it| it == "java-syntax"),
            "the xml queries affected java-syntax: {affected:?}"
        );
    }

    #[test]
    fn the_real_graph_stays_everything_for_a_root_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let graph = Workspace::load(&root).unwrap();
        let affected = graph.affected(&paths(&["Cargo.toml"]));
        assert_eq!(affected.len(), graph.packages.len());
        let affected = graph.affected(&paths(&["vendor/gpui-pre-windows/src/events.rs"]));
        assert_eq!(
            affected.len(),
            graph.packages.len(),
            "a vendored dependency reaches the crates through the manifest"
        );
    }

    #[test]
    fn the_real_graph_links_text_core_to_the_editor_view_and_panels() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let graph = Workspace::load(&root).unwrap();
        let affected = graph.affected(&paths(&["crates/editor/text-core/src/lib.rs"]));
        for name in ["text-core", "editor-view", "panels"] {
            assert!(
                affected.iter().any(|it| it == name),
                "{name} is not affected by text-core: {affected:?}"
            );
        }
        let affected = graph.affected(&paths(&["crates/services/updater/src/lib.rs"]));
        assert!(
            !affected.iter().any(|it| it == "text-core"),
            "updater affected text-core: {affected:?}"
        );
    }
}
