//! Finding the JDK for jdtls, jdtls itself and java-debug without fixed paths.
//!
//! The same order as `run-arro.cmd`: the variables of the environment win, then
//! `tools\` next to `arro.exe`, then `IDE_TOOLS` and its `tools`, then the usual
//! install folders of a JDK. The script stays for users who start it, so both must
//! find the same pair.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use java_project::feature_of;
use jdtls_adapter::MIN_JAVA_VERSION;

use crate::session::{DEBUG_PLUGIN, JAVA_HOME, JDTLS_HOME};

/// The three paths jdtls needs: the JDK it runs on, its own folder and the jar of
/// java-debug.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    pub java_home: PathBuf,
    pub jdtls_home: PathBuf,
    pub debug_plugin: PathBuf,
}

/// What the search did not find, in the order the message reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The server itself.
    Jdtls,
    /// A JDK new enough to run the server.
    Jdk,
    /// The java-debug jar of the debugger.
    Debug,
}

impl Missing {
    /// The short line of the status bar.
    pub fn status_line(self) -> String {
        match self {
            Missing::Jdtls => "Java: jdtls not found".to_owned(),
            Missing::Jdk => format!("Java: JDK {MIN_JAVA_VERSION} not found"),
            Missing::Debug => "Java: java-debug not found".to_owned(),
        }
    }

    /// What to install and where, for the balloon that a click on the status line
    /// opens. The last sentence points at the README of the distribution.
    pub fn details(self) -> String {
        let readme = "See README.txt next to arro.exe or https://github.com/PabloSensei/arro.";
        match self {
            Missing::Jdtls => format!(
                "jdtls is the Eclipse JDT Language Server, which gives Arro its Java semantics.\n\
                 Put the unpacked release in tools\\jdtls-<version>\\ next to arro.exe, or set \
                 {JDTLS_HOME} to its folder.\n{readme}"
            ),
            Missing::Jdk => format!(
                "jdtls needs JDK {min} or newer. The project builds on its own JDK, so this one \
                 only has to start the server.\nSet {JAVA_HOME} to a JDK {min} or newer, or \
                 install one under C:\\Program Files\\Amazon Corretto, C:\\Program Files\\Java, \
                 C:\\Program Files\\Eclipse Adoptium or %LOCALAPPDATA%\\Programs; JAVA_HOME is \
                 read last.\n{readme}",
                min = MIN_JAVA_VERSION
            ),
            Missing::Debug => format!(
                "java-debug is the debugger of Java projects: \
                 com.microsoft.java.debug.plugin-*.jar from vscode-java-debug.\nPut it in \
                 tools\\vscode-java-debug-<version>\\extension\\server\\ next to arro.exe, or set \
                 {DEBUG_PLUGIN} to the jar.\n{readme}"
            ),
        }
    }
}

/// Where the search looks. The application fills it from the environment, a test
/// from a table, so the same code runs without a machine that has the tools.
#[derive(Clone, Debug, Default)]
pub struct Search {
    /// The folder of `arro.exe`: `tools\` next to it comes first.
    pub exe_dir: Option<PathBuf>,
    /// `IDE_JAVA_HOME`, taken as it is.
    pub java_home: Option<PathBuf>,
    /// `IDE_JDTLS_HOME`, taken as it is.
    pub jdtls_home: Option<PathBuf>,
    /// `IDE_JAVA_DEBUG_PLUGIN`, taken as it is.
    pub debug_plugin: Option<PathBuf>,
    /// `IDE_TOOLS`, a folder with `jdtls-*` or with a `tools` folder that has them.
    pub tools: Option<PathBuf>,
    /// The folders whose `jdk*` subfolders are JDK installs.
    pub jdk_roots: Vec<PathBuf>,
    /// `JAVA_HOME`, the last place a JDK is looked for.
    pub project_java_home: Option<PathBuf>,
}

impl Search {
    /// The search of this machine: the variables first, then the usual install
    /// folders. `exe_dir` is the folder of `arro.exe`.
    pub fn from_env(exe_dir: Option<&Path>) -> Self {
        let var = |name| env::var_os(name).map(PathBuf::from);
        let program_files = var("IDE_PROGRAM_FILES").or_else(|| var("ProgramFiles"));
        let mut jdk_roots = Vec::new();
        if let Some(base) = program_files {
            jdk_roots.push(base.join("Amazon Corretto"));
            jdk_roots.push(base.join("Java"));
            jdk_roots.push(base.join("Eclipse Adoptium"));
        }
        if let Some(base) = var("LOCALAPPDATA") {
            jdk_roots.push(base.join("Programs"));
        }
        Self {
            exe_dir: exe_dir.map(Path::to_path_buf),
            java_home: var(JAVA_HOME),
            jdtls_home: var(JDTLS_HOME),
            debug_plugin: var(DEBUG_PLUGIN),
            tools: var("IDE_TOOLS"),
            jdk_roots,
            project_java_home: var("JAVA_HOME"),
        }
    }

    /// The folders that hold `jdtls-*` and `vscode-java-debug-*`: next to `arro.exe`
    /// first, then `IDE_TOOLS` and its `tools`.
    fn tool_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(exe_dir) = &self.exe_dir {
            dirs.push(exe_dir.join("tools"));
        }
        if let Some(tools) = &self.tools {
            dirs.push(tools.clone());
            dirs.push(tools.join("tools"));
        }
        dirs
    }
}

/// Finds the three paths, or says which one is missing: jdtls first, then its JDK,
/// then java-debug, the same order as the script.
pub fn discover(search: &Search) -> Result<Toolchain, Missing> {
    let tool_dirs = search.tool_dirs();
    let jdtls_home = match &search.jdtls_home {
        Some(home) => home.clone(),
        None => tool_dirs
            .iter()
            .find_map(|dir| newest_named(dir, "jdtls-"))
            .ok_or(Missing::Jdtls)?,
    };
    let java_home = match &search.java_home {
        Some(home) => home.clone(),
        None => find_jdk(search).ok_or(Missing::Jdk)?,
    };
    let debug_plugin = match &search.debug_plugin {
        Some(jar) => jar.clone(),
        None => tool_dirs
            .iter()
            .find_map(|dir| newest_debug_plugin(dir))
            .ok_or(Missing::Debug)?,
    };
    Ok(Toolchain {
        java_home,
        jdtls_home,
        debug_plugin,
    })
}

/// The newest `jdtls-*` folder of `root`, as the name orders it: `dir /o-n` of the
/// script keeps the first of that order.
fn newest_named(root: &Path, prefix: &str) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = entries(root)
        .into_iter()
        .filter(|path| path.is_dir() && name_is(path).starts_with(prefix))
        .collect();
    found.sort_by_key(|path| name_is(path));
    found.pop()
}

/// The newest `vscode-java-debug-*` folder that has a plugin jar: the first folder of
/// the name order wins, and inside it the newest jar.
fn newest_debug_plugin(root: &Path) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = entries(root)
        .into_iter()
        .filter(|path| path.is_dir() && name_is(path).starts_with("vscode-java-debug-"))
        .collect();
    versions.sort_by_key(|path| name_is(path));
    versions.into_iter().rev().find_map(|dir| {
        let mut jars: Vec<PathBuf> = entries(&dir.join("extension").join("server"))
            .into_iter()
            .filter(|path| {
                let name = name_is(path);
                path.is_file()
                    && name.starts_with("com.microsoft.java.debug.plugin-")
                    && name.ends_with(".jar")
            })
            .collect();
        jars.sort_by_key(|path| name_is(path));
        jars.pop()
    })
}

/// The newest JDK of [`MIN_JAVA_VERSION`] or newer under the install roots; `JAVA_HOME`
/// counts only when nothing is installed, the same rule as the script.
fn find_jdk(search: &Search) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    for root in &search.jdk_roots {
        let mut homes: Vec<PathBuf> = entries(root)
            .into_iter()
            .filter(|path| path.is_dir() && name_is(path).starts_with("jdk"))
            .collect();
        homes.sort_by_key(|path| name_is(path));
        for home in homes {
            let Some(feature) = release_feature(&home) else {
                continue;
            };
            if feature < MIN_JAVA_VERSION {
                continue;
            }
            if best.as_ref().is_none_or(|(major, _)| feature > *major) {
                best = Some((feature, home));
            }
        }
    }
    best.map(|(_, home)| home).or_else(|| {
        let home = search.project_java_home.clone()?;
        (release_feature(&home)? >= MIN_JAVA_VERSION).then_some(home)
    })
}

/// `JAVA_VERSION` of the `release` file: `1.8.0_492` is 8, `25.0.1` is 25.
fn release_feature(home: &Path) -> Option<u32> {
    let release = fs::read_to_string(home.join("release")).ok()?;
    let version = release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?;
    feature_of(version.trim().trim_matches('"'))
}

/// The entries of a folder, empty when it does not exist.
fn entries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries.flatten().map(|entry| entry.path()).collect()
}

/// A file name in the order Windows shows it: without case.
fn name_is(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn scratch(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("java-lang-setup-{}-{name}", process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn release(home: &Path, version: &str) {
        fs::create_dir_all(home).unwrap();
        fs::write(
            home.join("release"),
            format!("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
    }

    fn jdtls(tools: &Path, version: &str) -> PathBuf {
        let home = tools.join(format!("jdtls-{version}"));
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn debug_jar(tools: &Path, version: &str) -> PathBuf {
        let jar = tools
            .join(format!("vscode-java-debug-{version}"))
            .join("extension")
            .join("server")
            .join(format!("com.microsoft.java.debug.plugin-{version}.jar"));
        fs::create_dir_all(jar.parent().unwrap()).unwrap();
        fs::write(&jar, "").unwrap();
        jar
    }

    fn search(exe_dir: &Path) -> Search {
        Search {
            exe_dir: Some(exe_dir.to_path_buf()),
            ..Search::default()
        }
    }

    #[test]
    fn the_tools_next_to_the_exe_are_found() {
        let exe = scratch("next-to-exe");
        let tools = exe.join("tools");
        let jdtls_home = jdtls(&tools, "1.63.0");
        let debug_plugin = debug_jar(&tools, "0.60.0");
        let jdks = scratch("next-to-exe-jdks");
        let java_home = jdks.join("jdk-25.0.1");
        release(&java_home, "25.0.1");
        let mut found = search(&exe);
        found.jdk_roots = vec![jdks];

        let toolchain = discover(&found).unwrap();

        assert_eq!(toolchain.jdtls_home, jdtls_home);
        assert_eq!(toolchain.debug_plugin, debug_plugin);
        assert_eq!(toolchain.java_home, java_home);
    }

    #[test]
    fn the_newest_jdtls_by_name_wins() {
        let exe = scratch("two-jdtls");
        let tools = exe.join("tools");
        jdtls(&tools, "1.62.0");
        let newest = jdtls(&tools, "1.63.0");
        let mut found = search(&exe);
        found.java_home = Some(PathBuf::from(r"C:\chosen\jdk"));
        found.debug_plugin = Some(PathBuf::from(r"C:\chosen\debug.jar"));

        let toolchain = discover(&found).unwrap();

        assert_eq!(toolchain.jdtls_home, newest);
    }

    #[test]
    fn the_newest_jdk_of_the_install_roots_wins() {
        let exe = scratch("three-jdks");
        let jdks = scratch("three-jdks-roots");
        release(&jdks.join("jdk-21"), "21.0.2");
        release(&jdks.join("jdk-25"), "25.0.1");
        let newest = jdks.join("jdk-26");
        release(&newest, "26");
        let mut found = search(&exe);
        found.jdtls_home = Some(PathBuf::from(r"C:\chosen\jdtls"));
        found.debug_plugin = Some(PathBuf::from(r"C:\chosen\debug.jar"));
        found.jdk_roots = vec![jdks];

        let toolchain = discover(&found).unwrap();

        assert_eq!(toolchain.java_home, newest);
    }

    #[test]
    fn java_home_counts_only_when_it_is_new_enough() {
        let exe = scratch("java-home");
        let jdks = scratch("java-home-roots");
        release(&jdks.join("jdk-21"), "21.0.2");
        let project = jdks.join("project-jdk");
        release(&project, "1.8.0_492");
        let mut found = search(&exe);
        found.jdtls_home = Some(PathBuf::from(r"C:\chosen\jdtls"));
        found.debug_plugin = Some(PathBuf::from(r"C:\chosen\debug.jar"));
        found.jdk_roots = vec![jdks];
        found.project_java_home = Some(project.clone());

        let missing = discover(&found).unwrap_err();
        assert_eq!(missing, Missing::Jdk);
        assert_eq!(missing.status_line(), "Java: JDK 25 not found");

        let newer = scratch("java-home-newer");
        let project = newer.join("project-jdk");
        release(&project, "25.0.3");
        found.jdk_roots = vec![newer];
        found.project_java_home = Some(project.clone());
        assert_eq!(discover(&found).unwrap().java_home, project);
    }

    #[test]
    fn ide_tools_comes_after_the_folder_next_to_the_exe() {
        let exe = scratch("ide-tools");
        let beside = jdtls(&exe.join("tools"), "1.62.0");
        let ide_tools = scratch("ide-tools-folder");
        let in_tools = jdtls(&ide_tools.join("tools"), "1.70.0");
        let mut found = search(&exe);
        found.java_home = Some(PathBuf::from(r"C:\chosen\jdk"));
        found.debug_plugin = Some(PathBuf::from(r"C:\chosen\debug.jar"));
        found.tools = Some(ide_tools);

        assert_eq!(discover(&found).unwrap().jdtls_home, beside);

        found.exe_dir = None;
        let toolchain = discover(&found).unwrap();
        assert_eq!(toolchain.jdtls_home, in_tools);
    }

    #[test]
    fn the_variables_win_over_the_search() {
        let exe = scratch("variables");
        let tools = exe.join("tools");
        jdtls(&tools, "1.63.0");
        debug_jar(&tools, "0.60.0");
        let jdks = scratch("variables-jdks");
        release(&jdks.join("jdk-26"), "26");
        let chosen = exe.join("chosen");
        let mut found = search(&exe);
        found.jdtls_home = Some(chosen.join("jdtls"));
        found.java_home = Some(chosen.join("jdk"));
        found.debug_plugin = Some(chosen.join("debug.jar"));
        found.jdk_roots = vec![jdks];

        let toolchain = discover(&found).unwrap();

        assert_eq!(toolchain.jdtls_home, chosen.join("jdtls"));
        assert_eq!(toolchain.java_home, chosen.join("jdk"));
        assert_eq!(toolchain.debug_plugin, chosen.join("debug.jar"));
    }

    #[test]
    fn every_missing_tool_has_its_own_line_and_explanation() {
        let exe = scratch("messages");
        let found = search(&exe);

        let missing = discover(&found).unwrap_err();
        assert_eq!(missing, Missing::Jdtls);
        assert_eq!(missing.status_line(), "Java: jdtls not found");

        let mut found = search(&exe);
        found.jdtls_home = Some(PathBuf::from(r"C:\chosen\jdtls"));
        let missing = discover(&found).unwrap_err();
        assert_eq!(missing, Missing::Jdk);
        assert_eq!(missing.status_line(), "Java: JDK 25 not found");

        let mut found = search(&exe);
        found.jdtls_home = Some(PathBuf::from(r"C:\chosen\jdtls"));
        found.java_home = Some(PathBuf::from(r"C:\chosen\jdk"));
        let missing = discover(&found).unwrap_err();
        assert_eq!(missing, Missing::Debug);
        assert_eq!(missing.status_line(), "Java: java-debug not found");
    }

    /// The folder names `xtask dist` writes into `<out>\tools`: jdtls 1.63.0 and
    /// java-debug 0.60.0 with the plugin jar of its release.
    fn dist_layout(tools: &Path) -> (PathBuf, PathBuf) {
        let jdtls = tools.join("jdtls-1.63.0");
        let debug = tools
            .join("vscode-java-debug-0.60.0")
            .join("extension")
            .join("server")
            .join("com.microsoft.java.debug.plugin-0.53.2.jar");
        (jdtls, debug)
    }

    #[test]
    fn the_layout_of_the_distribution_is_found() {
        let exe = scratch("dist-layout");
        let (jdtls_home, debug_plugin) = dist_layout(&exe.join("tools"));
        fs::create_dir_all(jdtls_home.join("config_win")).unwrap();
        fs::create_dir_all(jdtls_home.join("plugins")).unwrap();
        fs::create_dir_all(debug_plugin.parent().unwrap()).unwrap();
        fs::write(&debug_plugin, "").unwrap();
        let jdks = scratch("dist-layout-jdks");
        let java_home = jdks.join("jdk-25");
        release(&java_home, "25.0.1");
        let mut found = search(&exe);
        found.jdk_roots = vec![jdks];

        let toolchain = discover(&found).unwrap();

        assert_eq!(toolchain.jdtls_home, jdtls_home);
        assert_eq!(toolchain.debug_plugin, debug_plugin);
        assert_eq!(toolchain.java_home, java_home);
    }

    #[test]
    fn the_explanation_says_where_the_tool_goes_and_which_variable_sets_it() {
        let jdtls = Missing::Jdtls.details();
        assert!(jdtls.contains(r"tools\jdtls-<version>\"), "{jdtls}");
        assert!(jdtls.contains("IDE_JDTLS_HOME"), "{jdtls}");

        let jdk = Missing::Jdk.details();
        assert!(jdk.contains("IDE_JAVA_HOME"), "{jdk}");
        assert!(jdk.contains("Program Files\\Java"), "{jdk}");
        assert!(jdk.contains("JAVA_HOME"), "{jdk}");

        let debug = Missing::Debug.details();
        assert!(debug.contains("IDE_JAVA_DEBUG_PLUGIN"), "{debug}");
        assert!(
            debug.contains(r"tools\vscode-java-debug-<version>\extension\server\"),
            "{debug}"
        );
        for text in [jdtls, jdk, debug] {
            assert!(text.contains("README.txt"), "{text}");
            assert!(text.contains("github.com/PabloSensei/arro"), "{text}");
            assert!(
                !text.contains("set IDE_JDTLS_HOME and IDE_JAVA_HOME"),
                "{text}"
            );
        }
    }
}
