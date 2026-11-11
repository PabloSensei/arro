use std::time::Duration;

use base::Result;
use java_project::{BuildEnv, Gradle, Jdk, Maven, TestHook, TestSelection};
use project_model::ProjectModel;
use runner::{RunPlan, TestDebug, TestListen, TestProvider, TestRequest};

/// Ctrl+C first: Maven ends its forks, the Gradle client tells its server to cancel the
/// build and the server ends the test JVMs.
const GRACE: Duration = Duration::from_secs(10);
/// What a test JVM of either tool prints when its debug agent listens.
const DEBUG_READY_LINE: &str = "Listening for transport dt_socket at address:";

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
    ) -> Result<RunPlan> {
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
        let program = match model.tool.as_str() {
            "maven" => match &agent {
                Some(agent) => {
                    Maven::new(env).debug_test_command(&model.root, &selection, &hook, agent)?
                }
                None => Maven::new(env).test_command(&model.root, &selection, &hook)?,
            },
            "gradle" => match &agent {
                Some(agent) => {
                    Gradle::new(env).debug_test_command(&model.root, &selection, &hook, agent)?
                }
                None => Gradle::new(env).test_command(&model.root, &selection, &hook)?,
            },
            tool => return Err(format!("no test runs for the tool \"{tool}\"").into()),
        };
        Ok(RunPlan {
            before: None,
            program,
            grace: GRACE,
        })
    }
}

impl TestProvider for JavaTestProvider {
    fn plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> Result<RunPlan> {
        self.plan_with(model, request, listen, None)
    }

    fn debug_plan(
        &self,
        model: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
        debug: &TestDebug,
    ) -> Result<RunPlan> {
        self.plan_with(model, request, listen, Some(debug))
    }

    fn debug_ready(&self) -> Option<&'static str> {
        Some(DEBUG_READY_LINE)
    }
}
