//! The bridge between the MCP tools that build, test and run the project and the Run
//! window. A tool runs on the thread of the server, so it leaves a job here and waits; the
//! Run panel takes the job in its poll, starts the same run the buttons start and answers
//! when the run ends. Nothing in this module knows a language: it speaks the neutral types
//! of `tool_gateway`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tool_gateway::{
    BuildOutcome, BuildRequest, ProjectRunner, RunConfiguration, RunOutput, SessionScope,
    StartRunRequest, TestsOutcome, TestsRequest, ToolError,
};

/// How long a tool waits for the Run window before it gives up. A build of a big project
/// or a full test run takes minutes; the window polls the queue every moment it is open.
const ANSWER_WAIT: Duration = Duration::from_secs(600);

/// What a tool asks the Run window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// Builds the project or one module of it.
    Build(BuildRequest),
    /// Runs tests.
    Tests(TestsRequest),
    /// Lists the configurations the user can start.
    Configurations,
    /// Starts a configuration, with the overrides of this run when it has any.
    Start(StartRunRequest),
    /// The tail of a run by id.
    Output(String),
    /// Stops a run by id.
    Stop(String),
}

impl Request {
    /// Whether the request starts something and takes time; those go one at a time, the
    /// reading and stopping ones answer at once.
    fn takes_the_window(&self) -> bool {
        matches!(self, Self::Build(_) | Self::Tests(_) | Self::Start(_))
    }
}

/// What the Run window answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Build(BuildOutcome),
    Tests(TestsOutcome),
    Configurations(Vec<RunConfiguration>),
    Started(String),
    Output(RunOutput),
    Stopped,
}

/// A job the Run window took and will answer; a dropped sender means the window went away.
pub(crate) struct Job {
    /// The number of the job; the output of the run carries `tool-{id}`.
    pub(crate) id: u64,
    pub(crate) request: Request,
    /// The name of the client that asked, empty for the window itself.
    pub(crate) client: Option<String>,
    pub(crate) reply: mpsc::Sender<Result<Answer, String>>,
}

impl Job {
    /// The id `get_run_output` reads the log of this job by.
    pub(crate) fn output_id(&self) -> String {
        format!("tool-{}", self.id)
    }
}

#[derive(Default)]
struct State {
    next: u64,
    waiting: VecDeque<Job>,
    /// A job that runs something took the window and has not ended yet.
    taken: bool,
}

/// The queue of the window. One long job at a time: the second one waits for the first,
/// as a second Run button press does.
#[derive(Default)]
pub(crate) struct RunBridge {
    state: Mutex<State>,
}

impl RunBridge {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The next job whose turn it is. The panel calls this on every poll.
    pub(crate) fn take_job(&self) -> Option<Job> {
        let mut state = self.lock();
        let index = state
            .waiting
            .iter()
            .position(|job| !state.taken || !job.request.takes_the_window())?;
        let job = state.waiting.remove(index)?;
        if job.request.takes_the_window() {
            state.taken = true;
        }
        Some(job)
    }

    /// The long job ended, so the next one may start.
    pub(crate) fn release(&self) {
        self.lock().taken = false;
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn submit(&self, request: Request, client: Option<&str>) -> Result<Answer, ToolError> {
        let (reply, answered) = mpsc::channel();
        {
            let mut state = self.lock();
            state.next += 1;
            let id = state.next;
            state.waiting.push_back(Job {
                id,
                request,
                client: client.map(str::to_string),
                reply,
            });
        }
        match answered.recv_timeout(ANSWER_WAIT) {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(message)) => Err(ToolError::Failed(message)),
            Err(RecvTimeoutError::Timeout) => Err(ToolError::Failed(
                "the Run window did not answer in time".to_string(),
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err(ToolError::Failed("the Run window is closed".to_string()))
            }
        }
    }
}

impl ProjectRunner for RunBridge {
    fn build(
        &self,
        _scope: &SessionScope,
        request: &BuildRequest,
    ) -> Result<BuildOutcome, ToolError> {
        match self.submit(Request::Build(request.clone()), None)? {
            Answer::Build(outcome) => Ok(outcome),
            _ => Err(ToolError::Failed("the build answered wrongly".to_string())),
        }
    }

    fn test(
        &self,
        _scope: &SessionScope,
        request: &TestsRequest,
    ) -> Result<TestsOutcome, ToolError> {
        match self.submit(Request::Tests(request.clone()), None)? {
            Answer::Tests(outcome) => Ok(outcome),
            _ => Err(ToolError::Failed("the tests answered wrongly".to_string())),
        }
    }

    fn configurations(&self, _scope: &SessionScope) -> Result<Vec<RunConfiguration>, ToolError> {
        match self.submit(Request::Configurations, None)? {
            Answer::Configurations(configurations) => Ok(configurations),
            _ => Err(ToolError::Failed(
                "the configuration list answered wrongly".to_string(),
            )),
        }
    }

    fn start(
        &self,
        _scope: &SessionScope,
        request: &StartRunRequest,
        client: Option<&str>,
    ) -> Result<String, ToolError> {
        match self.submit(Request::Start(request.clone()), client)? {
            Answer::Started(id) => Ok(id),
            _ => Err(ToolError::Failed("the run answered wrongly".to_string())),
        }
    }

    fn output(&self, _scope: &SessionScope, id: &str) -> Result<RunOutput, ToolError> {
        match self.submit(Request::Output(id.to_string()), None)? {
            Answer::Output(output) => Ok(output),
            _ => Err(ToolError::Failed("the output answered wrongly".to_string())),
        }
    }

    fn stop(&self, _scope: &SessionScope, id: &str) -> Result<(), ToolError> {
        match self.submit(Request::Stop(id.to_string()), None)? {
            Answer::Stopped => Ok(()),
            _ => Err(ToolError::Failed("the stop answered wrongly".to_string())),
        }
    }
}
