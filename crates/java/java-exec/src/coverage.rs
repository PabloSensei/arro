//! Run with Coverage for Java: where the agent writes its data, the helper that reads it
//! back into the model of the window, and the paths of the classes it analyses.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base::{Cancel, Result};
use java_project::BuildEnv;
use lang_api::{
    BranchCoverage, Counters, CoverageReport, FileCoverage, LineCoverage, TypeCoverage,
};
use proc_sup::{ProcessSpec, capture};
use project_model::ProjectModel;
use serde::Deserialize;

/// The folder under the cache root that holds the data of the coverage runs.
const COVERAGE_FOLDER: &str = "coverage";
/// The program of the helper that reads the data of an agent.
const REPORT_MAIN: &str = "ide.coverage.Report";

/// A fresh data file for a run: the old ones are removed, so the folder never keeps the
/// data of runs the user forgot about.
pub fn exec_file(cache_root: &Path, name: &str) -> Result<PathBuf> {
    let directory = cache_root.join(COVERAGE_FOLDER);
    fs::create_dir_all(&directory)?;
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "exec")
            {
                fs::remove_file(path).ok();
            }
        }
    }
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    Ok(directory.join(format!("{stem}-{millis}.exec")))
}

/// The directories of the classes of the project that exist on disk.
fn class_dirs(model: &ProjectModel) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for module in &model.modules {
        for unit in &module.units {
            for dir in &unit.output_dirs {
                if dir.is_dir() {
                    dirs.push(dir.clone());
                }
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Reads the data an agent wrote into `exec` with the report helper of the tools.
pub fn report(env: &BuildEnv, exec: &Path, model: &ProjectModel) -> Result<CoverageReport> {
    let tools = env.coverage_tools()?;
    let dirs = class_dirs(model);
    if dirs.is_empty() {
        return Err(format!(
            "nothing to analyse: the project has no built classes under {}",
            model.root.display()
        )
        .into());
    }
    let jdk = env.project_jdk(&model.root)?;
    let classpath = format!("{}/*", tools.report.display());
    let mut spec = ProcessSpec::new(jdk.java())
        .cwd(model.root.clone())
        .args(["-cp", &classpath, REPORT_MAIN])
        .arg(exec);
    for dir in &dirs {
        spec = spec.arg(dir);
    }
    let captured = capture(&env.apply(spec, &jdk), &Cancel::new())
        .map_err(|error| format!("no coverage report: {error}"))?;
    if !captured.status.success() {
        let mut text = captured.stderr;
        text.push_str(&captured.stdout);
        let tail: Vec<&str> = text.lines().collect();
        let tail = tail[tail.len().saturating_sub(40)..].join("\n");
        return Err(format!(
            "no coverage report: exit {:?}\n{tail}",
            captured.status.code()
        )
        .into());
    }
    parse_report(model, &captured.stdout)
}

/// Turns the JSON of the helper into the model of the window. The sources are relative to
/// the root of the build: each is looked for under the source roots of the model, so a
/// file of `src/` becomes its absolute path.
fn parse_report(model: &ProjectModel, json: &str) -> Result<CoverageReport> {
    let raw: RawReport =
        serde_json::from_str(json).map_err(|error| format!("bad coverage report: {error}"))?;
    let files = raw
        .files
        .into_iter()
        .map(|file| FileCoverage {
            path: source_path(model, &file.source),
            lines: file
                .lines
                .into_iter()
                .map(|line| LineCoverage {
                    line: line.line,
                    hits: line.hits,
                    branches: line
                        .branches
                        .map(|[covered, total]| BranchCoverage { covered, total }),
                })
                .collect(),
        })
        .collect();
    let types = raw
        .types
        .into_iter()
        .map(|kind| {
            // The helper names a source by its package, as a class file does. The window
            // opens it, so it has to point at the real file under a source root.
            let absolute = source_path(model, &kind.source);
            let source = absolute
                .strip_prefix(&model.root)
                .map_or_else(|_| PathBuf::from(&kind.source), Path::to_path_buf);
            TypeCoverage {
                name: kind.name,
                package: kind.package,
                source,
                class: counters(kind.class),
                method: counters(kind.method),
                line: counters(kind.line),
                branch: counters(kind.branch),
            }
        })
        .collect();
    Ok(CoverageReport {
        root: model.root.clone(),
        files,
        types,
    })
}

/// The absolute source of a file a report names by its package path.
fn source_path(model: &ProjectModel, source: &str) -> PathBuf {
    for module in &model.modules {
        for unit in &module.units {
            for root in &unit.source_roots {
                let path = root.join(source);
                if path.is_file() {
                    return path;
                }
            }
        }
    }
    model.root.join(source)
}

fn counters([covered, total]: [u32; 2]) -> Counters {
    Counters { covered, total }
}

#[derive(Deserialize)]
struct RawReport {
    files: Vec<RawFile>,
    types: Vec<RawType>,
}

#[derive(Deserialize)]
struct RawFile {
    source: String,
    lines: Vec<RawLine>,
}

#[derive(Deserialize)]
struct RawLine {
    line: u32,
    hits: u32,
    #[serde(default)]
    branches: Option<[u32; 2]>,
}

#[derive(Deserialize)]
struct RawType {
    name: String,
    package: String,
    source: String,
    class: [u32; 2],
    method: [u32; 2],
    line: [u32; 2],
    branch: [u32; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "files": [{"source": "demo/Counter.java", "lines": [
            {"line": 7, "hits": 3},
            {"line": 16, "hits": 1, "branches": [1, 2]}]}],
        "types": [{"name": "demo.Counter", "package": "demo", "source": "demo/Counter.java",
                   "class": [1, 1], "method": [2, 3], "line": [5, 6], "branch": [1, 2]}]
    }"#;

    fn model(root: &Path) -> ProjectModel {
        ProjectModel {
            root: root.to_owned(),
            ..ProjectModel::default()
        }
    }

    #[test]
    fn the_report_of_the_helper_keeps_lines_branches_and_counters() {
        let report = parse_report(&model(Path::new("C:/p")), JSON).unwrap();

        let file = report.file(Path::new("C:/p/demo/Counter.java")).unwrap();
        assert_eq!(file.lines[0].hits, 3);
        assert_eq!(file.lines[1].branches.unwrap().covered, 1);
        let kind = &report.types[0];
        assert_eq!(kind.name, "demo.Counter");
        assert_eq!(
            kind.method,
            Counters {
                covered: 2,
                total: 3
            }
        );
        assert_eq!(
            kind.branch,
            Counters {
                covered: 1,
                total: 2
            }
        );
        assert_eq!(
            report
                .source(Path::new("demo/Counter.java"))
                .unwrap()
                .line_counters(),
            Counters {
                covered: 2,
                total: 2
            }
        );
    }

    #[test]
    fn a_broken_report_is_an_error() {
        assert!(parse_report(&model(Path::new("C:/p")), "not json").is_err());
    }

    #[test]
    fn a_source_found_under_a_source_root_gets_its_absolute_path() {
        use project_model::{Module, Unit};

        let root = test_support::scratch("coverage-sources");
        let sources = root.join("src");
        fs::create_dir_all(sources.join("demo")).unwrap();
        fs::write(sources.join("demo/Counter.java"), "class Counter {}\n").unwrap();
        let model = ProjectModel {
            root: root.clone(),
            modules: vec![Module {
                units: vec![Unit {
                    source_roots: vec![sources.clone()],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        let report = parse_report(&model, JSON).unwrap();

        assert_eq!(report.files[0].path, sources.join("demo/Counter.java"));
        assert_eq!(
            report.types[0].source,
            PathBuf::from("src/demo/Counter.java"),
            "the tree opens the source under its root"
        );
    }

    #[test]
    fn the_data_file_is_fresh_and_named_after_the_configuration() {
        let root = test_support::scratch("coverage-exec");
        let old = root.join("coverage/old.exec");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, b"old").unwrap();

        let exec = exec_file(&root, "My App").unwrap();

        assert_eq!(exec.parent().unwrap(), root.join("coverage"));
        assert!(!old.exists(), "the data of the earlier run is gone");
        assert!(
            exec.file_stem()
                .unwrap()
                .to_string_lossy()
                .starts_with("My-App-")
        );
    }

    #[test]
    fn the_class_folders_of_the_model_are_the_ones_on_disk() {
        use project_model::{Module, Unit};

        let root = test_support::scratch("coverage-classes");
        let present = root.join("out");
        fs::create_dir_all(&present).unwrap();
        let model = ProjectModel {
            modules: vec![Module {
                units: vec![Unit {
                    output_dirs: vec![present.clone(), root.join("missing")],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        assert_eq!(class_dirs(&model), vec![present]);
    }
}
