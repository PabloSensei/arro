//! The bridge between the MCP debug tools and the Debug window. A tool runs on the
//! thread of the server, so it leaves a job here and waits; the Debug panel takes the
//! job in its poll and answers from the session it draws, or asks the Run window to
//! start a test or a configuration the way the Debug button does. Nothing in this module
//! knows a language: it speaks the neutral types of `tool_gateway`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tool_gateway::{
    BreakpointId, BreakpointInfo, BreakpointQuery, DebugAnswer, DebugControl, DebugEvaluateQuery,
    DebugQuery, DebugRunOutcome, DebugRunRequest, DebugSessionState, DebugStack, DebugStackQuery,
    DebugStep, DebugStepQuery, DebugValue, DebugVariables, DebugVariablesQuery, DebugWaitQuery,
    SessionScope, ToolError,
};

/// How long a tool waits for an answer of the window that is not a wait of its own.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// How long the window has to take a job before the tool gives up. A start may wait for a
/// build, so it is patient; the answers of a stopped session come at once.
const START_WAIT: Duration = Duration::from_secs(120);

/// What a tool asks the Debug window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    SetBreakpoint(BreakpointQuery),
    RemoveBreakpoint(BreakpointId),
    Breakpoints,
    /// Starts a test or a configuration under the debugger.
    Run(DebugRunRequest),
    Wait(DebugWaitQuery),
    Stack(DebugStackQuery),
    Variables(DebugVariablesQuery),
    Evaluate(DebugEvaluateQuery),
    Step {
        step: DebugStep,
        seconds: u64,
    },
    Stop,
    /// A tool of the session set of IDEA, which the window answers from its Debug panel.
    Debug(DebugQuery),
}

/// What the Debug window answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Breakpoint(BreakpointInfo),
    Breakpoints(Vec<BreakpointInfo>),
    Removed,
    Started(String),
    State(DebugSessionState),
    Stack(DebugStack),
    Variables(DebugVariables),
    Value(DebugValue),
    Stopped,
    Debug(DebugAnswer),
}

/// A job the window took and will answer; a dropped sender means the window went away.
pub(crate) struct Job {
    pub(crate) request: Request,
    /// The name of the client that asked, empty for the window itself.
    pub(crate) client: Option<String>,
    pub(crate) reply: mpsc::Sender<Result<Answer, String>>,
}

impl Job {
    pub(crate) fn answer(self, answer: Answer) {
        let _ = self.reply.send(Ok(answer));
    }

    pub(crate) fn fail(self, message: impl Into<String>) {
        let _ = self.reply.send(Err(message.into()));
    }
}

#[derive(Default)]
struct State {
    waiting: VecDeque<Job>,
}

/// The queue of the window. Every job is answered by a poll; the ones that wait for a
/// stop stay in the panel until the session moves.
#[derive(Default)]
pub(crate) struct DebugBridge {
    state: Mutex<State>,
}

impl DebugBridge {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The next job the window has to take. The panel calls this on every poll.
    pub(crate) fn take_job(&self) -> Option<Job> {
        self.lock().waiting.pop_front()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn submit(
        &self,
        request: Request,
        client: Option<&str>,
        wait: Duration,
    ) -> Result<Answer, ToolError> {
        let (reply, answered) = mpsc::channel();
        self.lock().waiting.push_back(Job {
            request,
            client: client.map(str::to_string),
            reply,
        });
        match answered.recv_timeout(wait) {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(message)) => Err(ToolError::Failed(message)),
            Err(RecvTimeoutError::Timeout) => Err(ToolError::Failed(
                "the Debug window did not answer in time".to_string(),
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err(ToolError::Failed("the Debug window is closed".to_string()))
            }
        }
    }
}

/// How long a tool of the window waits when it asks the session to move: the seconds of
/// the tool and a little for the window to notice the stop.
fn wait_of(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, 120) + 10)
}

impl DebugControl for DebugBridge {
    fn set_breakpoint(
        &self,
        _scope: &SessionScope,
        query: &BreakpointQuery,
    ) -> Result<BreakpointInfo, ToolError> {
        match self.submit(Request::SetBreakpoint(query.clone()), None, ANSWER_WAIT)? {
            Answer::Breakpoint(info) => Ok(info),
            _ => Err(ToolError::Failed(
                "the breakpoint answered wrongly".to_string(),
            )),
        }
    }

    fn remove_breakpoint(&self, _scope: &SessionScope, id: &BreakpointId) -> Result<(), ToolError> {
        match self.submit(Request::RemoveBreakpoint(id.clone()), None, ANSWER_WAIT)? {
            Answer::Removed => Ok(()),
            _ => Err(ToolError::Failed(
                "the breakpoint answered wrongly".to_string(),
            )),
        }
    }

    fn breakpoints(&self, _scope: &SessionScope) -> Result<Vec<BreakpointInfo>, ToolError> {
        match self.submit(Request::Breakpoints, None, ANSWER_WAIT)? {
            Answer::Breakpoints(items) => Ok(items),
            _ => Err(ToolError::Failed(
                "the breakpoint list answered wrongly".to_string(),
            )),
        }
    }

    fn run(
        &self,
        scope: &SessionScope,
        request: &DebugRunRequest,
        client: Option<&str>,
    ) -> Result<DebugRunOutcome, ToolError> {
        let session = match self.submit(Request::Run(request.clone()), client, START_WAIT)? {
            Answer::Started(session) => session,
            _ => {
                return Err(ToolError::Failed(
                    "the debug session answered wrongly".to_string(),
                ));
            }
        };
        let query = DebugWaitQuery {
            seconds: request.wait_seconds,
        };
        let state = self.wait(scope, &query)?;
        Ok(DebugRunOutcome { session, state })
    }

    fn wait(
        &self,
        _scope: &SessionScope,
        query: &DebugWaitQuery,
    ) -> Result<DebugSessionState, ToolError> {
        match self.submit(Request::Wait(query.clone()), None, wait_of(query.seconds))? {
            Answer::State(state) => Ok(state),
            _ => Err(ToolError::Failed(
                "the debug session answered wrongly".to_string(),
            )),
        }
    }

    fn stack(
        &self,
        _scope: &SessionScope,
        query: &DebugStackQuery,
    ) -> Result<DebugStack, ToolError> {
        match self.submit(Request::Stack(query.clone()), None, ANSWER_WAIT)? {
            Answer::Stack(stack) => Ok(stack),
            _ => Err(ToolError::Failed("the stack answered wrongly".to_string())),
        }
    }

    fn variables(
        &self,
        _scope: &SessionScope,
        query: &DebugVariablesQuery,
    ) -> Result<DebugVariables, ToolError> {
        match self.submit(Request::Variables(query.clone()), None, ANSWER_WAIT)? {
            Answer::Variables(variables) => Ok(variables),
            _ => Err(ToolError::Failed(
                "the variables answered wrongly".to_string(),
            )),
        }
    }

    fn evaluate(
        &self,
        _scope: &SessionScope,
        query: &DebugEvaluateQuery,
    ) -> Result<DebugValue, ToolError> {
        match self.submit(Request::Evaluate(query.clone()), None, ANSWER_WAIT)? {
            Answer::Value(value) => Ok(value),
            _ => Err(ToolError::Failed(
                "the expression answered wrongly".to_string(),
            )),
        }
    }

    fn step(
        &self,
        _scope: &SessionScope,
        query: &DebugStepQuery,
    ) -> Result<DebugSessionState, ToolError> {
        let request = Request::Step {
            step: query.step,
            seconds: query.seconds,
        };
        match self.submit(request, None, wait_of(query.seconds))? {
            Answer::State(state) => Ok(state),
            _ => Err(ToolError::Failed("the step answered wrongly".to_string())),
        }
    }

    fn stop(&self, _scope: &SessionScope) -> Result<(), ToolError> {
        match self.submit(Request::Stop, None, ANSWER_WAIT)? {
            Answer::Stopped => Ok(()),
            _ => Err(ToolError::Failed("the stop answered wrongly".to_string())),
        }
    }

    fn call(&self, scope: &SessionScope, query: &DebugQuery) -> Result<DebugAnswer, ToolError> {
        match query {
            DebugQuery::Start(query) => self.start_session(scope, query),
            DebugQuery::Frames(query) => self.frames(scope, query),
            DebugQuery::Values(query) => self.values(scope, query),
            DebugQuery::Control(query) => {
                let wait = Duration::from_millis(query.timeout_ms.min(600_000)) + ANSWER_WAIT;
                self.submit_debug(DebugQuery::Control(query.clone()), wait)
            }
            DebugQuery::RunToLine(query) => {
                let wait = Duration::from_millis(query.timeout_ms.min(600_000)) + ANSWER_WAIT;
                self.submit_debug(DebugQuery::RunToLine(query.clone()), wait)
            }
            other => self.submit_debug(other.clone(), ANSWER_WAIT),
        }
    }
}

impl DebugBridge {
    /// Sends a query to the Debug panel and waits for its answer.
    fn submit_debug(&self, query: DebugQuery, wait: Duration) -> Result<DebugAnswer, ToolError> {
        match self.submit(Request::Debug(query), None, wait)? {
            Answer::Debug(answer) => Ok(answer),
            _ => Err(ToolError::Failed(
                "the debug window answered the session tools wrongly".to_string(),
            )),
        }
    }

    /// Starts a session, waits for a pause for as long as `graceWaitMs` asks and answers
    /// the session as it stands then.
    fn start_session(
        &self,
        scope: &SessionScope,
        query: &tool_gateway::DebugStartQuery,
    ) -> Result<DebugAnswer, ToolError> {
        let session = match self.submit(
            Request::Debug(DebugQuery::Start(query.clone())),
            query.client.as_deref(),
            START_WAIT,
        )? {
            Answer::Debug(DebugAnswer::Started(session)) => session,
            Answer::Debug(other) => return Ok(other),
            _ => {
                return Err(ToolError::Failed(
                    "the debug window answered the start wrongly".to_string(),
                ));
            }
        };
        let control = tool_gateway::DebugControlQuery {
            session_id: Some(session.id.clone()),
            action: tool_gateway::DebugControlAction::WaitForPause,
            timeout_ms: query.grace_wait_ms,
            events_limit: 0,
        };
        // A session that does not pause within the grace is still a session that started.
        let _ = self.call(scope, &DebugQuery::Control(control));
        let status = self.call(scope, &DebugQuery::Status)?;
        let DebugAnswer::Status(status) = status else {
            return Ok(DebugAnswer::Started(session));
        };
        let found = status
            .sessions
            .into_iter()
            .find(|known| known.id == session.id)
            .unwrap_or(session);
        Ok(DebugAnswer::Started(found))
    }

    /// The frames of a thread, read through the stack of the Debug window. A thread named
    /// by its shown name is looked up first.
    fn frames(
        &self,
        scope: &SessionScope,
        query: &tool_gateway::DebugFramesQuery,
    ) -> Result<DebugAnswer, ToolError> {
        let thread = match &query.thread {
            None => None,
            Some(tool_gateway::DebugThreadSelector::Id(id)) => Some(*id),
            Some(tool_gateway::DebugThreadSelector::Name(name)) => {
                let threads = self.call(
                    scope,
                    &DebugQuery::Threads(tool_gateway::DebugThreadsQuery {
                        session_id: query.session_id.clone(),
                        limit: 200,
                        offset: 0,
                    }),
                )?;
                let DebugAnswer::Threads(page) = threads else {
                    return Err(ToolError::Failed(
                        "the debug window answered the threads wrongly".to_string(),
                    ));
                };
                page.threads
                    .iter()
                    .find(|thread| &thread.name == name || thread.id.to_string() == *name)
                    .map(|thread| thread.id)
                    .ok_or_else(|| ToolError::BadInput(format!("no thread is shown as {name}")))?
                    .into()
            }
        };
        let stack = self.stack(
            scope,
            &DebugStackQuery {
                thread,
                frames: query.offset.saturating_add(query.limit).max(1),
            },
        )?;
        let frames = stack
            .frames
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .map(|frame| tool_gateway::DebugFrameInfo {
                index: frame.index,
                file: frame.path,
                line: frame.line,
                is_current: frame.index == query.offset && query.offset == 0,
                presentation: frame.name,
            })
            .collect();
        Ok(DebugAnswer::Frames(tool_gateway::DebugFramePage {
            thread_id: stack.thread.as_ref().map(|thread| thread.id),
            thread_name: stack.thread.as_ref().map(|thread| thread.name.clone()),
            frames,
            total: stack.total,
        }))
    }

    /// The values of a frame, read through the variables of the Debug window and shown as
    /// the one tree IDEA returns.
    fn values(
        &self,
        scope: &SessionScope,
        query: &tool_gateway::DebugValuesQuery,
    ) -> Result<DebugAnswer, ToolError> {
        let variables = self.variables(
            scope,
            &DebugVariablesQuery {
                frame: query.frame_index,
                depth: query.depth,
                length: 2_000,
                variables: 200,
            },
        )?;
        let mut values = Vec::new();
        for scope in variables.scopes {
            for variable in scope.variables {
                values.push(node_of(variable));
            }
        }
        Ok(DebugAnswer::Values(tool_gateway::DebugFrameValues {
            frame_index: query.frame_index,
            values,
        }))
    }
}

/// A variable of the window as the tree of an answer.
fn node_of(variable: tool_gateway::DebugVariable) -> tool_gateway::DebugValueNode {
    tool_gateway::DebugValueNode {
        name: variable.name,
        value: variable.value,
        type_name: variable.type_name,
        children: variable.children.into_iter().map(node_of).collect(),
    }
}
