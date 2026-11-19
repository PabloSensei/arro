use std::path::Path;

use base::Cancel;
use lang_api::{
    Answer, Confidence, Provider, Reply, RunPoint, RunPointKind, RunPointQuery, SourceId,
};

use crate::project::JavaProject;

const SOURCE: SourceId = SourceId::new("language");

/// The version whose simple `main` the parser knows. A project that builds with an
/// older version keeps the class `main`, and the extra mark does no harm.
const SIMPLE_MAIN_FEATURE: u32 = 25;

impl Provider<RunPointQuery, Vec<RunPoint>> for JavaProject {
    fn start(&self, query: RunPointQuery, _: Cancel, mut reply: Reply<Vec<RunPoint>>) {
        let points = run_points_of(&query.0);
        let _ = reply.send(Answer::new(points, Confidence::Certain, SOURCE));
    }
}

fn run_points_of(file: &Path) -> Vec<RunPoint> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    for entry in java_exec::parse_entry_points(&text, file, SIMPLE_MAIN_FEATURE) {
        points.push(RunPoint {
            kind: RunPointKind::Program,
            name: entry.class.clone(),
            container: entry.class,
            file: entry.file,
            line: entry.type_line.unwrap_or(entry.line),
            framework: entry.spring_boot,
        });
    }
    for class in java_exec::parse_tests(&text, file) {
        points.push(RunPoint {
            kind: RunPointKind::Test,
            name: class.class.clone(),
            container: class.class.clone(),
            file: class.file.clone(),
            line: class.line,
            framework: false,
        });
        for method in class.methods {
            points.push(RunPoint {
                kind: RunPointKind::Test,
                name: format!("{}.{}", class.class, method.name),
                container: class.class.clone(),
                file: class.file.clone(),
                line: method.line,
                framework: false,
            });
        }
    }
    points.sort_by(|left, right| (left.line, &left.name).cmp(&(right.line, &right.name)));
    points
}
