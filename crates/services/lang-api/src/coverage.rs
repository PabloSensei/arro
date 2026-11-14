//! What a coverage run measured: the lines of a source file, the branches of a line and
//! the counters of a type. The language brings the numbers, the core knows nothing about
//! how they were collected.

use std::path::{Path, PathBuf};

/// The branches of one line: how many of them a run took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BranchCoverage {
    pub covered: u32,
    pub total: u32,
}

/// How a line of a source looks after a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineState {
    /// The line ran.
    Covered,
    /// The line ran, but a branch of it was not taken.
    Partial,
    /// The line did not run.
    NotCovered,
}

/// What a run measured on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineCoverage {
    /// One-based, as a report names lines.
    pub line: u32,
    /// The instructions of the line that ran.
    pub hits: u32,
    pub branches: Option<BranchCoverage>,
}

impl LineCoverage {
    pub fn state(&self) -> LineState {
        if self.hits == 0 {
            LineState::NotCovered
        } else if self
            .branches
            .is_some_and(|branches| branches.covered < branches.total)
        {
            LineState::Partial
        } else {
            LineState::Covered
        }
    }
}

/// A counted part of a type, such as its methods: how many there are and how many ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub covered: u32,
    pub total: u32,
}

impl Counters {
    /// The percentage the window shows, `None` when there is nothing to cover.
    pub fn percent(&self) -> Option<u32> {
        (self.total > 0).then(|| self.covered * 100 / self.total)
    }
}

/// The lines of one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCoverage {
    /// The absolute path of the source.
    pub path: PathBuf,
    pub lines: Vec<LineCoverage>,
}

impl FileCoverage {
    pub fn line(&self, line: u32) -> Option<&LineCoverage> {
        self.lines.iter().find(|coverage| coverage.line == line)
    }

    /// The lines that ran, over all lines a run could see.
    pub fn line_counters(&self) -> Counters {
        Counters {
            covered: self.lines.iter().filter(|line| line.hits > 0).count() as u32,
            total: self.lines.len() as u32,
        }
    }
}

/// The counters of one type, for the tree of the Coverage window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeCoverage {
    /// The full name, `demo.Counter`.
    pub name: String,
    /// The package, `demo`.
    pub package: String,
    /// The source relative to the root of the project, `demo/Counter.src`.
    pub source: PathBuf,
    pub class: Counters,
    pub method: Counters,
    pub line: Counters,
    pub branch: Counters,
}

/// What one coverage run measured over the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageReport {
    /// The root the relative sources of the types are under.
    pub root: PathBuf,
    pub files: Vec<FileCoverage>,
    pub types: Vec<TypeCoverage>,
}

impl CoverageReport {
    /// The lines of the file at `path`, the absolute path of the source.
    pub fn file(&self, path: &Path) -> Option<&FileCoverage> {
        self.files.iter().find(|file| file.path == path)
    }

    /// The lines of a type whose source is relative to the root of the project.
    pub fn source(&self, source: &Path) -> Option<&FileCoverage> {
        let path = self.root.join(source);
        self.file(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(hits: u32, branches: Option<(u32, u32)>) -> LineCoverage {
        LineCoverage {
            line: 1,
            hits,
            branches: branches.map(|(covered, total)| BranchCoverage { covered, total }),
        }
    }

    #[test]
    fn a_line_that_did_not_run_is_not_covered() {
        assert_eq!(line(0, None).state(), LineState::NotCovered);
        assert_eq!(line(0, Some((0, 2))).state(), LineState::NotCovered);
    }

    #[test]
    fn a_line_with_a_branch_left_out_is_partial() {
        assert_eq!(line(3, None).state(), LineState::Covered);
        assert_eq!(line(1, Some((1, 2))).state(), LineState::Partial);
        assert_eq!(line(2, Some((2, 2))).state(), LineState::Covered);
        assert_eq!(line(2, Some((0, 0))).state(), LineState::Covered);
    }

    #[test]
    fn percentages_are_whole_numbers_and_none_without_anything_to_cover() {
        assert_eq!(
            Counters {
                covered: 1,
                total: 3
            }
            .percent(),
            Some(33)
        );
        assert_eq!(
            Counters {
                covered: 3,
                total: 6
            }
            .percent(),
            Some(50)
        );
        assert_eq!(
            Counters {
                covered: 0,
                total: 0
            }
            .percent(),
            None
        );
        assert_eq!(
            Counters {
                covered: 1,
                total: 1
            }
            .percent(),
            Some(100)
        );
    }

    #[test]
    fn a_file_counts_its_covered_lines() {
        let file = FileCoverage {
            path: PathBuf::from("C:/p/src/demo/Counter.src"),
            lines: vec![line(0, None), line(1, None), line(4, Some((1, 2)))],
        };

        assert_eq!(file.line(1), Some(&line(0, None)));
        assert_eq!(file.line(9), None);
        assert_eq!(
            file.line_counters(),
            Counters {
                covered: 2,
                total: 3
            }
        );
    }

    #[test]
    fn a_file_is_found_by_its_path_and_by_its_source_under_the_root() {
        let report = CoverageReport {
            root: PathBuf::from("C:/p"),
            files: vec![FileCoverage {
                path: PathBuf::from("C:/p/src/demo/Counter.src"),
                lines: vec![],
            }],
            types: vec![],
        };

        assert!(
            report
                .file(Path::new("C:/p/src/demo/Counter.src"))
                .is_some()
        );
        assert!(report.source(Path::new("src/demo/Counter.src")).is_some());
        assert!(report.file(Path::new("C:/p/other.src")).is_none());
    }
}
