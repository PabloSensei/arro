//! The coverage side of a run: the plan that adds the agent and the report the provider
//! reads back. The core keeps only the path of the data file and the numbers a provider
//! gives it, so no tool of a language appears here.

use std::path::Path;
use std::sync::Arc;

use base::Result;
use lang_api::CoverageReport;
use project_model::ProjectModel;

use crate::{RunPlan, TestProvider, TestRequest, TestRun};

/// A run whose JVM writes its coverage data to `exec`.
pub struct CoveragePlan {
    pub plan: RunPlan,
    /// Where the agent writes its data.
    pub exec: std::path::PathBuf,
    /// What the user has to know about this run, such as replaced project settings.
    pub note: Option<String>,
}

/// What a started coverage run reports to: the configuration provider or the test
/// provider that made the plan.
#[derive(Clone)]
pub enum CoverageOwner {
    Configuration(Arc<dyn crate::RunConfigProvider>),
    Tests(Arc<dyn TestProvider>),
}

impl CoverageOwner {
    /// Reads the data an agent left in `exec` into the model of the window.
    pub fn report(&self, exec: &Path, model: &ProjectModel) -> Result<CoverageReport> {
        match self {
            Self::Configuration(provider) => provider.coverage_report(exec, model),
            Self::Tests(provider) => provider.coverage_report(exec, model),
        }
    }
}

/// A started test run that collects coverage, with the file its data goes to.
pub struct TestCoverageRun {
    pub run: TestRun,
    pub exec: std::path::PathBuf,
    pub note: Option<String>,
}

impl TestRun {
    /// Starts the tests as [`TestRun::start`] does, with the coverage agent in their JVM.
    pub fn coverage_start(
        provider: &dyn TestProvider,
        model: &ProjectModel,
        request: &TestRequest,
    ) -> Result<TestCoverageRun> {
        let (run, coverage) = Self::start_with(provider, model, request, StartKind::Coverage)?;
        let coverage = coverage.expect("a coverage start returns its plan");
        Ok(TestCoverageRun {
            run,
            exec: coverage.exec,
            note: coverage.note,
        })
    }
}

/// The file a started coverage test run reports from, and what the user has to know.
pub(crate) struct CoverageStart {
    pub exec: std::path::PathBuf,
    pub note: Option<String>,
}

/// The kind of start of a test run, which decides the plan the provider makes.
pub(crate) enum StartKind<'a> {
    Plain,
    Debug(&'a crate::TestDebug),
    Coverage,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use proc_sup::ProcessSpec;
    use project_model::ProjectModel;

    use super::*;
    use crate::RunConfigProvider;
    #[cfg(windows)]
    use crate::TestListen;

    struct Fake {
        exec: PathBuf,
    }

    impl crate::RunConfigProvider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn discover(&self, _: &ProjectModel) -> Result<Vec<crate::RunConfig>> {
            Ok(Vec::new())
        }

        fn plan(&self, _: &crate::RunConfig, _: &ProjectModel) -> Result<RunPlan> {
            Ok(plain())
        }

        fn coverage_plan(&self, _: &crate::RunConfig, _: &ProjectModel) -> Result<CoveragePlan> {
            Ok(CoveragePlan {
                plan: plain(),
                exec: self.exec.clone(),
                note: None,
            })
        }

        fn coverage_report(&self, exec: &Path, _: &ProjectModel) -> Result<CoverageReport> {
            Ok(CoverageReport {
                root: exec.parent().unwrap().to_owned(),
                files: Vec::new(),
                types: Vec::new(),
            })
        }
    }

    fn plain() -> RunPlan {
        RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        }
    }

    #[test]
    fn the_owner_reads_the_report_of_the_provider_that_made_the_plan() {
        let provider = Arc::new(Fake {
            exec: PathBuf::from("run.exec"),
        });
        let owner = CoverageOwner::Configuration(provider);

        let report = owner.report(Path::new("run.exec"), &ProjectModel::default());

        assert_eq!(report.unwrap().root, PathBuf::new());
    }

    #[test]
    fn a_provider_without_coverage_says_so() {
        struct Plain;

        impl crate::RunConfigProvider for Plain {
            fn id(&self) -> &'static str {
                "plain"
            }

            fn discover(&self, _: &ProjectModel) -> Result<Vec<crate::RunConfig>> {
                Ok(Vec::new())
            }

            fn plan(&self, _: &crate::RunConfig, _: &ProjectModel) -> Result<RunPlan> {
                Ok(plain())
            }
        }

        let error = Plain
            .coverage_plan(
                &crate::RunConfig {
                    name: "plain".into(),
                    provider: "plain".into(),
                    assemble_first: false,
                    settings: Default::default(),
                },
                &ProjectModel::default(),
            )
            .err()
            .unwrap();

        assert!(
            error.to_string().contains("cannot collect coverage"),
            "{error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_coverage_test_start_hands_over_the_file_of_the_agent() {
        struct Provider;

        impl TestProvider for Provider {
            fn plan(
                &self,
                _: &ProjectModel,
                _: &crate::TestRequest,
                _: &TestListen,
            ) -> Result<RunPlan> {
                Ok(plain())
            }

            fn coverage_plan(
                &self,
                _: &ProjectModel,
                _: &crate::TestRequest,
                _: &TestListen,
            ) -> Result<CoveragePlan> {
                Ok(CoveragePlan {
                    plan: plain(),
                    exec: PathBuf::from("tests.exec"),
                    note: Some("the argLine of the project is replaced".into()),
                })
            }
        }

        let started = TestRun::coverage_start(
            &Provider,
            &ProjectModel::default(),
            &crate::TestRequest::default(),
        )
        .unwrap();

        assert_eq!(started.exec, PathBuf::from("tests.exec"));
        assert!(started.note.is_some());
        started.run.stop();
    }
}
