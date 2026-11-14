use std::path::Path;
use std::time::Duration;

use base::Result;
use java_project::{BuildEnv, Gradle, Jdk, Maven, TestHook, TestSelection};
use project_model::ProjectModel;
use runner::{CoveragePlan, RunPlan, TestDebug, TestListen, TestProvider, TestRequest};

/// Ctrl+C first: Maven ends its forks, the Gradle client tells its server to cancel the
/// build and the server ends the test JVMs.
const GRACE: Duration = Duration::from_secs(10);
/// What a test JVM of either tool prints when its debug agent listens.
const DEBUG_READY_LINE: &str = "Listening for transport dt_socket at address:";

/// What a coverage plan adds to the JVMs of the tests.
struct CoverageJvm<'a> {
    agent: String,
    version: Option<String>,
    exec: &'a Path,
}

/// Runs tests through the build tool of the project, which compiles first.
pub struct JavaTestProvider {
    env: BuildEnv,
}

impl JavaTestProvider {
    pub fn new(env: BuildEnv) -> Self {
        Self { env }
    }

    fn selection(request: &TestRequest) -> Result<TestSelection> {
        if request.case.is_some() && request.suite.is_none() {
            return Err("a test method needs its class".into());
        }
        Ok(TestSelection {
            module: request.module.clone(),
            class: request.suite.clone(),
            method: request.case.clone(),
            cases: request.cases.clone(),
        })
    }

    fn plan_with(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
        debug: Option<&TestDebug>,
        coverage: Option<&CoverageJvm<'_>>,
    ) -> Result<(RunPlan, Option<String>)> {
        let selection = Self::selection(request)?;
        let hook = TestHook {
            port: listen.port,
            token: listen.token.clone(),
        };
        let env = self.env.clone();
        let agent = match debug {
            Some(debug) => {
                let jdk: Jdk = env.project_jdk(&model.root)?;
                let flags = crate::JvmFlags::for_jdk(jdk.feature);
                Some(flags.debug_agent(&debug.host, debug.port, true))
            }
            None => None,
        };
        let mut note = None;
        let program = match model.tool.as_str() {
            "maven" => {
                let maven = Maven::new(env);
                match (&agent, coverage) {
                    (Some(agent), _) => {
                        maven.debug_test_command(&model.root, &selection, &hook, agent)?
                    }
                    (None, Some(coverage)) => {
                        let (spec, warned) = maven.coverage_test_command(
                            &model.root,
                            &selection,
                            &hook,
                            &coverage.agent,
                            coverage.version.as_deref(),
                            coverage.exec,
                        )?;
                        note = warned;
                        spec
                    }
                    (None, None) => maven.test_command(&model.root, &selection, &hook)?,
                }
            }
            "gradle" => {
                let gradle = Gradle::new(env);
                match (&agent, coverage) {
                    (Some(agent), _) => {
                        gradle.debug_test_command(&model.root, &selection, &hook, agent)?
                    }
                    (None, Some(coverage)) => {
                        let (spec, warned) = gradle.coverage_test_command(
                            &model.root,
                            &selection,
                            &hook,
                            &coverage.agent,
                        )?;
                        note = warned;
                        spec
                    }
                    (None, None) => gradle.test_command(&model.root, &selection, &hook)?,
                }
            }
            tool => return Err(format!("no test runs for the tool \"{tool}\"").into()),
        };
        Ok((
            RunPlan {
                before: None,
                program,
                grace: GRACE,
            },
            note,
        ))
    }
}

impl TestProvider for JavaTestProvider {
    fn plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> Result<RunPlan> {
        self.plan_with(model, request, listen, None, None)
            .map(|(plan, _)| plan)
    }

    fn debug_plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
        debug: &TestDebug,
    ) -> Result<RunPlan> {
        self.plan_with(model, request, listen, Some(debug), None)
            .map(|(plan, _)| plan)
    }

    fn coverage_plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> Result<CoveragePlan> {
        let tools = self.env.coverage_tools()?;
        if !tools.agent.is_file() {
            return Err(format!("no coverage agent at {}", tools.agent.display()).into());
        }
        let exec = crate::coverage::exec_file(&self.env.cache_root(), "tests")?;
        let coverage = CoverageJvm {
            agent: tools.java_agent(&exec),
            version: tools.version(),
            exec: &exec,
        };
        let (plan, note) = self.plan_with(model, request, listen, None, Some(&coverage))?;
        Ok(CoveragePlan { plan, exec, note })
    }

    fn coverage_report(
        &self,
        exec: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::CoverageReport> {
        crate::coverage::report(&self.env, exec, model)
    }

    fn debug_ready(&self) -> Option<&'static str> {
        Some(DEBUG_READY_LINE)
    }
}
