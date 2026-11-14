//! Builds what runs inside project JVMs. CI has no JVM, so this is a separate command.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;

/// Helpers run inside project JVMs, which go down to Java 8.
const RELEASE: &str = "8";
const EXTENSION: &str = "maven-ext.jar";
const TEST_LISTENER: &str = "ide-test-listener.jar";

/// Builds the test listener, then compiles `helpers/maven-ext` against the libraries of a
/// Maven distribution.
pub fn run(root: &Path, out: &Path, maven_lib: Option<&Path>) -> Result {
    run_with(root, out, maven_lib, None)
}

/// The same with the jars of JaCoCo, which `--jacoco-lib` names; without them they are
/// looked for in the local Maven repositories. A machine without them builds every other
/// helper and says that the coverage one is missing.
pub fn run_with(
    root: &Path,
    out: &Path,
    maven_lib: Option<&Path>,
    jacoco_lib: Option<&Path>,
) -> Result {
    fs::create_dir_all(out)?;
    test_listener(root, out)?;
    let maven_lib = match maven_lib {
        Some(lib) => lib.to_owned(),
        None => find_maven_lib().ok_or(
            "no Maven 3.9 found in MAVEN_HOME, M2_HOME, PATH or the mvnw cache (MAVEN_USER_HOME or ~/.m2, wrapper/dists): set MAVEN_USER_HOME or pass --maven-lib DIR",
        )?,
    };
    let source = root.join("helpers/maven-ext/src");
    let classes = out.join("maven-ext-classes");
    if classes.exists() {
        fs::remove_dir_all(&classes)?;
    }
    fs::create_dir_all(&classes)?;

    let mut sources = Vec::new();
    collect_sources(&source, &mut sources)?;
    let mut javac = Command::new(tool("javac"));
    javac
        .args([
            "--release",
            RELEASE,
            "-Xlint:-options",
            "-nowarn",
            "-encoding",
            "UTF-8",
        ])
        .arg("-cp")
        .arg(maven_lib.join("*"))
        .arg("-d")
        .arg(&classes)
        .args(&sources);
    check(&mut javac)?;

    let index = Path::new("META-INF/sisu/javax.inject.Named");
    fs::create_dir_all(classes.join("META-INF/sisu"))?;
    fs::copy(source.join(index), classes.join(index))?;
    let jar_file = out.join(EXTENSION);
    let mut jar = Command::new(tool("jar"));
    jar.arg("--create")
        .arg("--file")
        .arg(&jar_file)
        .arg("-C")
        .arg(&classes)
        .arg(".");
    check(&mut jar)?;
    fs::remove_dir_all(&classes)?;
    println!("{}", jar_file.display());
    coverage_report(root, out, jacoco_lib)?;
    Ok(())
}

fn tool(name: &str) -> PathBuf {
    let from_home = env::var_os("JAVA_HOME").map(|home| {
        #[cfg(windows)]
        {
            let mut file = PathBuf::from(home).join("bin").join(name);
            file.set_extension("exe");
            file
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(home).join("bin").join(name)
        }
    });
    from_home
        .filter(|file| file.is_file())
        .unwrap_or_else(|| name.into())
}

fn check(command: &mut Command) -> Result {
    let status = command.status()?;
    if status.success() {
        return Ok(());
    }
    Err(format!("{:?} failed: {status}", command.get_program()).into())
}

fn collect_sources(directory: &Path, files: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_sources(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "java")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// An installed Maven first, then one the wrapper unpacked. The extension compiles
/// against the 3.9 API, a Maven 4 candidate does not go here.
fn find_maven_lib() -> Option<PathBuf> {
    let installed = ["MAVEN_HOME", "M2_HOME"]
        .iter()
        .filter_map(|name| env::var_os(name).map(PathBuf::from))
        .chain(mvn_in_path());
    let lib = installed
        .flat_map(|home| {
            [
                home.join("lib"),
                home.join("libexec").join("lib"),
                home.join("libexec"),
            ]
        })
        .find(|lib| is_maven_3_9(lib));
    lib.or_else(wrapper_lib)
}

/// `bin/mvn` on PATH, the home is one level up. A distribution installed by a package
/// manager is a symlink, so the home comes from the real file.
fn mvn_in_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let programs = ["mvn.cmd", "mvn.bat", "mvn"];
    for directory in env::split_paths(&path) {
        for name in programs {
            let file = directory.join(name);
            if !file.is_file() {
                continue;
            }
            let file = fs::canonicalize(&file).unwrap_or(file);
            let Some(home) = file.parent().and_then(Path::parent) else {
                continue;
            };
            if home.join("lib").is_dir()
                || home.join("libexec").join("lib").is_dir()
                || home.join("libexec").is_dir()
            {
                return Some(home.to_owned());
            }
        }
    }
    None
}

/// `dists/<version>/<hash>/lib` from the script wrapper, or
/// `dists/<version>-bin/<hash>/apache-maven-<version>/lib` from the jar one.
fn wrapper_lib() -> Option<PathBuf> {
    let home = env::var_os("MAVEN_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".m2"))
        })?;
    let mut candidates = Vec::new();
    for version in children(&home.join("wrapper/dists")) {
        for hash in children(&version) {
            candidates.push(hash.join("lib"));
            candidates.extend(children(&hash).into_iter().map(|inner| inner.join("lib")));
        }
    }
    candidates.sort();
    candidates.into_iter().rev().find(|lib| is_maven_3_9(lib))
}

fn children(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let paths = entries.flatten().map(|entry| entry.path());
    paths.filter(|path| path.is_dir()).collect()
}

fn is_maven_3_9(lib: &Path) -> bool {
    children_files(lib)
        .iter()
        .any(|name| name.starts_with("maven-core-3.9."))
}

fn children_files(directory: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let names = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned());
    names.collect()
}

/// Compiled with stubs of the JUnit Platform API, which stay out of the jar: the listener
/// links to the platform of the project at run time.
fn test_listener(root: &Path, out: &Path) -> Result {
    let helper = root.join("helpers/test-listener");
    let stubs = out.join("test-listener-stubs");
    let classes = out.join("test-listener-classes");
    for directory in [&stubs, &classes] {
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
        fs::create_dir_all(directory)?;
    }
    let mut stub_sources = Vec::new();
    collect_sources(&helper.join("stubs"), &mut stub_sources)?;
    check(javac(&stubs).args(&stub_sources))?;
    let mut sources = Vec::new();
    collect_sources(&helper.join("src"), &mut sources)?;
    check(javac(&classes).arg("-cp").arg(&stubs).args(&sources))?;

    let services = Path::new("META-INF/services/org.junit.platform.launcher.TestExecutionListener");
    fs::create_dir_all(classes.join("META-INF/services"))?;
    fs::copy(helper.join("src").join(services), classes.join(services))?;
    let jar_file = out.join(TEST_LISTENER);
    let mut jar = Command::new(tool("jar"));
    jar.arg("--create")
        .arg("--file")
        .arg(&jar_file)
        .arg("-C")
        .arg(&classes)
        .arg(".");
    check(&mut jar)?;
    fs::remove_dir_all(&stubs)?;
    fs::remove_dir_all(&classes)?;
    println!("{}", jar_file.display());
    Ok(())
}

fn javac(classes: &Path) -> Command {
    let mut javac = Command::new(tool("javac"));
    javac
        .args([
            "--release",
            RELEASE,
            "-Xlint:-options",
            "-nowarn",
            "-encoding",
            "UTF-8",
        ])
        .arg("-d")
        .arg(classes);
    javac
}

/// The helper that reads the data of a JaCoCo agent, with the jars it loads next to it.
const COVERAGE_FOLDER: &str = "coverage";
/// The parts of JaCoCo and ASM the report helper links to, as Maven names them.
const JACOCO_ARTIFACTS: [&str; 4] = [
    "org/jacoco/org.jacoco.core",
    "org/ow2/asm/asm",
    "org/ow2/asm/asm-commons",
    "org/ow2/asm/asm-tree",
];

fn coverage_report(root: &Path, out: &Path, jacoco_lib: Option<&Path>) -> Result {
    let jars = match jacoco_lib {
        Some(directory) => jars_in(directory),
        None => find_jacoco_jars(),
    };
    if jars.is_empty() {
        eprintln!(
            "warning: no JaCoCo jars found in the local Maven repositories; \
             coverage-report.jar is not built, pass --jacoco-lib DIR with org.jacoco.core and the ASM jars"
        );
        return Ok(());
    }
    let helper = root.join("helpers/coverage-report");
    let classes = out.join("coverage-report-classes");
    if classes.exists() {
        fs::remove_dir_all(&classes)?;
    }
    fs::create_dir_all(&classes)?;
    let mut sources = Vec::new();
    collect_sources(&helper.join("src"), &mut sources)?;
    let classpath = env::join_paths(&jars)?;
    check(javac(&classes).arg("-cp").arg(&classpath).args(&sources))?;
    let directory = out.join(COVERAGE_FOLDER);
    if directory.exists() {
        fs::remove_dir_all(&directory)?;
    }
    fs::create_dir_all(&directory)?;
    let jar_file = directory.join("coverage-report.jar");
    let mut jar = Command::new(tool("jar"));
    jar.arg("--create")
        .arg("--file")
        .arg(&jar_file)
        .arg("-C")
        .arg(&classes)
        .arg(".");
    check(&mut jar)?;
    fs::remove_dir_all(&classes)?;
    for jar in &jars {
        if let Some(name) = jar.file_name() {
            fs::copy(jar, directory.join(name))?;
        }
    }
    println!("{}", jar_file.display());
    Ok(())
}

fn jars_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut jars: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "jar")
        })
        .collect();
    jars.sort();
    jars
}

/// The newest jar of every part of JaCoCo in the local repositories of Maven.
fn find_jacoco_jars() -> Vec<PathBuf> {
    let mut repositories: Vec<PathBuf> = Vec::new();
    if let Some(home) = env::var_os("MAVEN_USER_HOME").map(PathBuf::from) {
        repositories.push(home.join("repository"));
    }
    if let Some(home) = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
    {
        repositories.push(home.join(".m2").join("repository"));
    }
    JACOCO_ARTIFACTS
        .iter()
        .filter_map(|artifact| newest_jar(&repositories, artifact))
        .collect()
}

fn newest_jar(repositories: &[PathBuf], artifact: &str) -> Option<PathBuf> {
    let mut jars = Vec::new();
    for repository in repositories {
        for version in children(&repository.join(artifact)) {
            jars.extend(
                fs::read_dir(&version)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_file()
                            && path.extension().is_some_and(|extension| extension == "jar")
                            && !path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .ends_with("-sources.jar")
                    }),
            );
        }
    }
    jars.sort_by_cached_key(|path| version_order(&name_of(path)));
    jars.pop()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The numbers of the version in a jar name: `asm-9.10.1.jar` is `[9, 10, 1]`.
fn version_order(name: &str) -> Vec<u64> {
    let tail = name.strip_suffix(".jar").unwrap_or(name);
    let tail = tail.rsplit('-').next().unwrap_or(tail);
    tail.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(u64::MAX))
        .collect()
}
