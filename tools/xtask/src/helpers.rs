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
    Ok(())
}

fn tool(name: &str) -> PathBuf {
    let from_home = env::var_os("JAVA_HOME").map(|home| {
        let mut file = PathBuf::from(home).join("bin").join(name);
        file.set_extension("exe");
        file
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
        .map(|home| home.join("lib"))
        .find(|lib| is_maven_3_9(lib));
    lib.or_else(wrapper_lib)
}

/// `bin/mvn.cmd` on PATH, the home is one level up.
fn mvn_in_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let mvn = env::split_paths(&path).map(|directory| directory.join("mvn.cmd"));
    mvn.into_iter()
        .find(|file| file.is_file())?
        .parent()?
        .parent()
        .map(Path::to_owned)
}

/// `dists/<version>/<hash>/lib` from the script wrapper, or
/// `dists/<version>-bin/<hash>/apache-maven-<version>/lib` from the jar one.
fn wrapper_lib() -> Option<PathBuf> {
    let home = env::var_os("MAVEN_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("USERPROFILE")?).join(".m2")))?;
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
