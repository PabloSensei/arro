//! One debugging session: a connection to a debug adapter and the model the window reads.
//!
//! The session task owns the connection. The window sends [`Command`]s and receives
//! [`Snapshot`]s, so a slow adapter never holds a frame.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::client::{Client, Event};
use crate::model::{
    Breakpoint, BreakpointKind, BreakpointSpec, Capabilities, Command, ConsoleLine,
    DataBreakpointSpec, DebugState, Evaluation, ExceptionFilter, Frame, FunctionBreakpointSpec,
    OutputLine, Scope, Snapshot, StepTarget, Thread, Variable, VariableError, WatchValue, line_of,
    source_path,
};

/// What starts a session. `attach_port` is the port the debuggee listens on.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// The address of the adapter itself, `host:port`.
    pub address: String,
    pub adapter_id: String,
    /// `projectName` and similar adapter fields, passed through on attach.
    pub project: Option<String>,
    /// The host of the debuggee, `127.0.0.1` when the caller did not say.
    pub attach_host: Option<String>,
    pub attach_port: Option<u16>,
    /// The process id of the debuggee, when the caller knows it.
    pub process_id: Option<u32>,
    /// Names the debuggee in the console and in the process list of the adapter.
    pub name: String,
    pub breakpoints: Vec<BreakpointSpec>,
    /// Method breakpoints, sent before the adapter resumes the program.
    pub function_breakpoints: Vec<FunctionBreakpointSpec>,
    /// Field watchpoints, sent before the adapter resumes the program.
    pub data_breakpoints: Vec<DataBreakpointSpec>,
    /// Filters of the adapter the session starts with.
    pub exception_filters: Vec<String>,
    pub watch: Vec<String>,
}

/// Reads and runs events until the session ends. The channel closes then.
pub async fn run(
    launch: Launch,
    commands: UnboundedReceiver<Command>,
    snapshots: Sender<Arc<Snapshot>>,
) -> Result<(), crate::client::Error> {
    run_with_output(launch, commands, None, snapshots).await
}

/// Like [`run`], with the streams of the debuggee that its process gave to the caller.
pub async fn run_with_output(
    launch: Launch,
    mut commands: UnboundedReceiver<Command>,
    output: Option<UnboundedReceiver<OutputLine>>,
    snapshots: Sender<Arc<Snapshot>>,
) -> Result<(), crate::client::Error> {
    let (client, mut events) = Client::connect(&launch.address).await?;
    let mut session = Session {
        client,
        state: Snapshot {
            state: DebugState::Starting,
            process_id: launch.process_id,
            watch: launch
                .watch
                .iter()
                .map(|expression| WatchValue {
                    expression: expression.clone(),
                    ..WatchValue::default()
                })
                .collect(),
            ..Snapshot::default()
        },
        watch: launch.watch.clone(),
        breakpoints: BTreeMap::new(),
        function_breakpoints: launch.function_breakpoints.clone(),
        data_breakpoints: launch.data_breakpoints.clone(),
        exception_filters: launch.exception_filters.iter().cloned().collect(),
        project: launch.project.clone(),
        attach_host: launch.attach_host.clone(),
        attach_port: launch.attach_port,
        name: launch.name.clone(),
        adapter_id: launch.adapter_id.clone(),
        output,
        publish: snapshots,
    };
    for spec in launch.breakpoints {
        if spec.line > 0 {
            session
                .breakpoints
                .entry(spec.file.clone())
                .or_default()
                .insert(spec.line, spec);
        }
    }
    session.publish();
    session.start(&mut events).await?;

    loop {
        let stepped = tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    session.finish();
                    return Ok(());
                };
                session.event(event).await?
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    session.disconnect().await;
                    return Ok(());
                };
                session.command(command).await?
            }
            line = output_line(&mut session.output) => {
                match line {
                    Some(line) => {
                        session.console(line.text, line.error);
                        session.publish();
                    }
                    None => session.output = None,
                }
                false
            }
        };
        if stepped {
            return Ok(());
        }
    }
}

/// Waits for the next line of the debuggee, forever when there is no stream.
async fn output_line(output: &mut Option<UnboundedReceiver<OutputLine>>) -> Option<OutputLine> {
    match output {
        Some(output) => output.recv().await,
        None => std::future::pending().await,
    }
}

struct Session {
    client: Client,
    state: Snapshot,
    watch: Vec<String>,
    /// Settings per file and line, as the window set them.
    breakpoints: BTreeMap<PathBuf, BTreeMap<u32, BreakpointSpec>>,
    /// Method breakpoints, as the window set them.
    function_breakpoints: Vec<FunctionBreakpointSpec>,
    /// Field watchpoints, as the window set them.
    data_breakpoints: Vec<DataBreakpointSpec>,
    exception_filters: BTreeSet<String>,
    project: Option<String>,
    attach_host: Option<String>,
    attach_port: Option<u16>,
    name: String,
    adapter_id: String,
    /// Lines of the debuggee that the adapter does not report.
    output: Option<UnboundedReceiver<OutputLine>>,
    publish: Sender<Arc<Snapshot>>,
}

impl Session {
    fn publish(&self) {
        let _ = self.publish.send(Arc::new(self.state.clone()));
    }

    fn finish(&mut self) {
        self.state.state = DebugState::Terminated;
        self.publish();
    }

    async fn start(
        &mut self,
        events: &mut UnboundedReceiver<Event>,
    ) -> Result<(), crate::client::Error> {
        let arguments = json!({
            "clientID": "arro",
            "clientName": "Arro",
            "adapterID": self.adapter_id,
            "locale": "en-US",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsVariableType": true,
            "supportsVariablePaging": false,
            "supportsRunInTerminalRequest": false,
            "supportsMemoryReferences": false,
            "supportsProgressReporting": false,
        });
        let capabilities = self.client.request("initialize", arguments).await?;
        self.state.capabilities = capabilities_of(&capabilities);
        self.state.exception_filters = filters_of(&capabilities);
        let mut arguments = json!({"name": self.name});
        if let Some(port) = self.attach_port {
            arguments["hostName"] = json!(
                self.attach_host
                    .as_deref()
                    .filter(|host| !host.is_empty())
                    .unwrap_or("127.0.0.1")
            );
            arguments["port"] = json!(port);
        }
        if let Some(project) = &self.project {
            arguments["projectName"] = json!(project);
        }
        // The adapter sends `initialized` only after it knows what to attach to.
        let _ = self.client.request("attach", arguments).await;
        loop {
            match events.recv().await {
                Some(Event::Initialized) => break,
                Some(Event::Closed) | None => {
                    self.finish();
                    return Ok(());
                }
                Some(event) => self.collect(event),
            }
        }
        self.send_exception_filters().await;
        self.send_all_breakpoints().await;
        let _ = self.client.request("configurationDone", json!({})).await;
        self.state.state = DebugState::Running;
        self.console("Attached".to_owned(), false);
        self.publish();
        Ok(())
    }

    /// Sends the breakpoints of one file and records what the adapter made of them.
    async fn send_breakpoints(&mut self, file: &Path) {
        let settings: Vec<BreakpointSpec> = self
            .breakpoints
            .get(file)
            .map(|lines| {
                lines
                    .values()
                    .filter(|spec| spec.enabled && spec.line > 0)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let requested = settings
            .iter()
            .map(|spec| {
                let mut entry = json!({"line": spec.line});
                if let Some(condition) = spec.condition.as_deref().filter(|text| !text.is_empty()) {
                    entry["condition"] = json!(condition);
                }
                if let Some(hit) = spec
                    .hit_condition
                    .as_deref()
                    .filter(|text| !text.is_empty())
                {
                    entry["hitCondition"] = json!(hit);
                }
                if let Some(message) = spec.log_message.as_deref().filter(|text| !text.is_empty()) {
                    entry["logMessage"] = json!(message);
                }
                entry
            })
            .collect::<Vec<_>>();
        let lines: Vec<u32> = settings.iter().map(|spec| spec.line).collect();
        let arguments = json!({
            "source": {"path": file.to_string_lossy()},
            "breakpoints": requested,
            "lines": lines,
            "sourceModified": false,
        });
        let answer = self.client.request("setBreakpoints", arguments).await;
        self.state
            .breakpoints
            .retain(|breakpoint| breakpoint.file != file);
        match answer {
            Ok(body) => {
                let listed = body
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (index, item) in listed.iter().enumerate() {
                    let line = line_of(item);
                    let line = if line == 0 {
                        lines.get(index).copied().unwrap_or(0)
                    } else {
                        line
                    };
                    let message = item
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Some(message) = &message {
                        self.console(format!("{}:{line}: {message}", file.display()), true);
                    }
                    self.state.breakpoints.push(Breakpoint {
                        file: file.to_path_buf(),
                        line,
                        kind: BreakpointKind::Line,
                        id: item.get("id").and_then(Value::as_i64),
                        verified: item
                            .get("verified")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        message,
                    });
                }
                // An adapter that answers with nothing still has the breakpoints set.
                if listed.is_empty() {
                    for line in lines {
                        self.state.breakpoints.push(Breakpoint {
                            file: file.to_path_buf(),
                            line,
                            kind: BreakpointKind::Line,
                            id: None,
                            verified: true,
                            message: None,
                        });
                    }
                }
            }
            Err(error) => {
                let message = format!("{}: {error}", file.display());
                self.console(message, true);
                for line in lines {
                    self.state.breakpoints.push(Breakpoint {
                        file: file.to_path_buf(),
                        line,
                        kind: BreakpointKind::Line,
                        id: None,
                        verified: false,
                        message: Some("not accepted".into()),
                    });
                }
            }
        }
    }

    async fn send_all_breakpoints(&mut self) {
        let files: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
        for file in files {
            self.send_breakpoints(&file).await;
        }
        self.send_function_breakpoints().await;
        self.send_data_breakpoints().await;
    }

    /// Sends every enabled method breakpoint and keeps what the adapter made of them.
    async fn send_function_breakpoints(&mut self) {
        let requested: Vec<Value> = self
            .function_breakpoints
            .iter()
            .filter(|spec| spec.enabled && !spec.name.trim().is_empty())
            .map(|spec| {
                let mut entry = json!({"name": spec.name});
                if let Some(condition) = spec.condition.as_deref().filter(|text| !text.is_empty()) {
                    entry["condition"] = json!(condition);
                }
                if let Some(hit) = spec
                    .hit_condition
                    .as_deref()
                    .filter(|text| !text.is_empty())
                {
                    entry["hitCondition"] = json!(hit);
                }
                entry
            })
            .collect();
        if !self.state.capabilities.function_breakpoints {
            if !requested.is_empty() {
                self.console(
                    "method breakpoints: the adapter does not support them".to_owned(),
                    true,
                );
            }
            return;
        }
        let answer = self
            .client
            .request("setFunctionBreakpoints", json!({"breakpoints": requested}))
            .await;
        self.state.function_breakpoints.clear();
        match answer {
            Ok(body) => {
                self.state.function_breakpoints =
                    breakpoint_answers(&body, BreakpointKind::Function)
            }
            Err(error) => self.console(format!("method breakpoints: {error}"), true),
        }
    }

    /// Sends every enabled field watchpoint and keeps what the adapter made of them.
    async fn send_data_breakpoints(&mut self) {
        let requested: Vec<Value> = self
            .data_breakpoints
            .iter()
            .filter(|spec| spec.enabled && !spec.data_id.trim().is_empty())
            .map(|spec| {
                let mut entry = json!({
                    "dataId": spec.data_id,
                    "accessType": spec.access_type.as_dap(),
                });
                if let Some(condition) = spec.condition.as_deref().filter(|text| !text.is_empty()) {
                    entry["condition"] = json!(condition);
                }
                if let Some(hit) = spec
                    .hit_condition
                    .as_deref()
                    .filter(|text| !text.is_empty())
                {
                    entry["hitCondition"] = json!(hit);
                }
                entry
            })
            .collect();
        if !self.state.capabilities.data_breakpoints {
            if !requested.is_empty() {
                self.console(
                    "field watchpoints: the adapter does not support them".to_owned(),
                    true,
                );
            }
            return;
        }
        let answer = self
            .client
            .request("setDataBreakpoints", json!({"breakpoints": requested}))
            .await;
        self.state.data_breakpoints.clear();
        match answer {
            Ok(body) => {
                self.state.data_breakpoints = breakpoint_answers(&body, BreakpointKind::Data)
            }
            Err(error) => self.console(format!("field watchpoints: {error}"), true),
        }
    }

    async fn send_exception_filters(&mut self) {
        let filters: Vec<String> = self.exception_filters.iter().cloned().collect();
        let arguments = json!({ "filters": filters });
        if let Err(error) = self
            .client
            .request("setExceptionBreakpoints", arguments)
            .await
        {
            self.console(format!("exception breakpoints: {error}"), true);
        }
    }

    /// Collects an event that arrived before the launch finished.
    fn collect(&mut self, event: Event) {
        if let Event::Output { category, text } = event {
            let error = category == "stderr";
            self.console(text, error);
        }
    }

    fn console(&mut self, text: String, error: bool) {
        for line in text.lines() {
            if !line.is_empty() {
                self.state
                    .console
                    .push(ConsoleLine::new(line.to_owned(), error));
            }
        }
        // The console is a view of the session, not a log that grows forever.
        const MAX: usize = 5_000;
        let extra = self.state.console.len().saturating_sub(MAX);
        if extra > 0 {
            self.state.console.drain(..extra);
        }
    }

    /// Returns `true` when the session is over.
    async fn event(&mut self, event: Event) -> Result<bool, crate::client::Error> {
        match event {
            Event::Stopped {
                reason, thread_id, ..
            } => {
                self.state.state = DebugState::Stopped {
                    reason: reason.clone(),
                };
                self.state.stops += 1;
                self.state.step_targets = None;
                self.state.evaluation = None;
                self.console(format!("Stopped: {reason}"), false);
                self.refresh_threads().await;
                let thread = thread_id.or(self.state.current_thread);
                self.state.current_thread = thread;
                if let Some(thread) = thread {
                    self.stack(thread).await;
                }
                self.evaluate_watch().await;
            }
            Event::Continued { .. } => {
                self.state.state = DebugState::Running;
                self.clear_stop();
            }
            Event::Thread { .. } => {
                // A debuggee may start thousands of virtual threads. Asking for the whole
                // list on every start event would flood the adapter and the window; the
                // list is read when the session stops.
                if !matches!(self.state.state, DebugState::Stopped { .. }) {
                    return Ok(false);
                }
                self.refresh_threads().await;
            }
            Event::Output { category, text } => {
                let error = category == "stderr";
                self.console(text, error);
            }
            Event::Breakpoint { body, .. } => self.apply_breakpoint_event(&body),
            Event::Exited { code } => {
                let error = code != 0;
                self.console(format!("The debuggee exited with code {code}"), error);
            }
            Event::Process {
                name,
                system_process_id,
            } => {
                self.state.process_id = system_process_id.or(self.state.process_id);
                let id = system_process_id.map_or_else(String::new, |id| format!(" ({id})"));
                self.console(format!("{name}{id}"), false);
            }
            Event::Terminated => {
                self.finish();
                return Ok(true);
            }
            Event::Initialized | Event::Other { .. } => {}
            Event::Closed => {
                self.finish();
                return Ok(true);
            }
        }
        self.publish();
        Ok(false)
    }

    /// An adapter may change a breakpoint later, a condition that does not compile for one.
    fn apply_breakpoint_event(&mut self, body: &Value) {
        let Some(item) = body.get("breakpoint") else {
            return;
        };
        let message = item
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let verified = item
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let id = item.get("id").and_then(Value::as_i64);
        let mut answered = false;
        if let Some(id) = id
            && let Some(breakpoint) = self
                .state
                .breakpoints
                .iter_mut()
                .chain(self.state.function_breakpoints.iter_mut())
                .chain(self.state.data_breakpoints.iter_mut())
                .find(|breakpoint| breakpoint.id == Some(id))
        {
            breakpoint.verified = verified;
            breakpoint.message = message.clone();
            answered = true;
        }
        let file = source_path(item.get("source")).or_else(|| {
            self.state
                .breakpoints
                .first()
                .map(|breakpoint| breakpoint.file.clone())
        });
        let Some(file) = file else {
            return;
        };
        let line = line_of(item);
        if !answered
            && let Some(breakpoint) = self
                .state
                .breakpoints
                .iter_mut()
                .find(|breakpoint| breakpoint.file == file && breakpoint.line == line)
        {
            breakpoint.verified = verified;
            breakpoint.message = message.clone();
        }
        if let Some(message) = message {
            self.console(format!("{}:{line}: {message}", file.display()), true);
        }
    }

    fn clear_stop(&mut self) {
        self.state.frames.clear();
        self.state.scopes.clear();
        self.state.selected_frame = None;
        self.state.variables.clear();
        self.state.step_targets = None;
        self.state.evaluation = None;
        for watch in &mut self.state.watch {
            watch.value.clear();
            watch.error = None;
        }
    }

    /// Returns `true` when the session is over.
    async fn command(&mut self, command: Command) -> Result<bool, crate::client::Error> {
        match command {
            Command::Continue => {
                let arguments = json!({"threadId": self.thread_or_zero()});
                let _ = self.client.request("continue", arguments).await;
                self.state.state = DebugState::Running;
                self.clear_stop();
            }
            Command::Pause => {
                let arguments = json!({"threadId": self.thread_or_zero()});
                let _ = self.client.request("pause", arguments).await;
            }
            Command::Next | Command::StepIn | Command::ForceStepIn | Command::StepOut => {
                let command = match command {
                    Command::Next => "next",
                    Command::StepIn | Command::ForceStepIn => "stepIn",
                    _ => "stepOut",
                };
                let arguments = json!({"threadId": self.thread_or_zero()});
                let _ = self.client.request(command, arguments).await;
                self.state.state = DebugState::Running;
                self.clear_stop();
            }
            Command::StepInTargets => {
                let frame = self.state.selected_frame.unwrap_or(0);
                let arguments = json!({"frameId": frame});
                let targets = match self.client.request("stepInTargets", arguments).await {
                    Ok(body) => step_targets_of(&body),
                    // An adapter without the request, or with nothing to offer: a plain
                    // step is the answer.
                    Err(_) => Vec::new(),
                };
                self.state.step_targets = Some(targets);
            }
            Command::StepInTarget(target) => {
                let arguments = json!({
                    "threadId": self.thread_or_zero(),
                    "targetId": target,
                });
                let _ = self.client.request("stepIn", arguments).await;
                self.state.state = DebugState::Running;
                self.state.step_targets = None;
                self.clear_stop();
            }
            Command::Disconnect => {
                self.disconnect().await;
                return Ok(true);
            }
            Command::SelectThread(thread) => {
                self.state.current_thread = Some(thread);
                self.stack(thread).await;
            }
            Command::SelectFrame(frame) => {
                self.state.selected_frame = Some(frame);
                self.scopes(frame).await;
            }
            Command::Expand(reference) => {
                let variables = self.variables(reference).await;
                self.state.variables.insert(reference, variables);
            }
            Command::SetBreakpoint(spec) => {
                if spec.line > 0 {
                    self.breakpoints
                        .entry(spec.file.clone())
                        .or_default()
                        .insert(spec.line, spec.clone());
                    self.send_breakpoints(&spec.file).await;
                }
            }
            Command::RemoveBreakpoint { file, line } => {
                if let Some(lines) = self.breakpoints.get_mut(&file) {
                    lines.remove(&line);
                }
                self.send_breakpoints(&file).await;
            }
            Command::SetExceptionBreakpoints(filters) => {
                self.exception_filters = filters.into_iter().collect();
                self.send_exception_filters().await;
            }
            Command::SetFunctionBreakpoints(specs) => {
                self.function_breakpoints = specs;
                self.send_function_breakpoints().await;
            }
            Command::SetDataBreakpoints(specs) => {
                self.data_breakpoints = specs;
                self.send_data_breakpoints().await;
            }
            Command::RestartFrame(frame) => {
                if let Err(error) = self
                    .client
                    .request("restartFrame", json!({"frameId": frame}))
                    .await
                {
                    self.console(format!("restart frame: {error}"), true);
                }
            }
            Command::AddWatch(expression) => {
                let expression = expression.trim().to_owned();
                if !expression.is_empty() && !self.watch.contains(&expression) {
                    self.watch.push(expression.clone());
                    self.state.watch.push(WatchValue {
                        expression,
                        ..WatchValue::default()
                    });
                    self.evaluate_watch().await;
                }
            }
            Command::RemoveWatch(expression) => {
                self.watch.retain(|item| item != &expression);
                self.state
                    .watch
                    .retain(|item| item.expression != expression);
            }
            Command::Evaluate(expression) => {
                let answer = self.evaluate(&expression, "repl").await;
                match answer {
                    Ok(value) => self.console(format!("> {expression}\n{value}"), false),
                    Err(error) => self.console(format!("> {expression}\n{error}"), true),
                }
            }
            Command::EvaluateLog(expression) => {
                let answer = self.evaluate(&expression, "repl").await;
                match answer {
                    Ok(value) => self.console(value, false),
                    Err(error) => self.console(format!("{expression}: {error}"), true),
                }
            }
            Command::EvaluateQuick(expression) => {
                self.state.evaluation = Some(self.evaluate_quick(&expression).await);
            }
            Command::SetVariable {
                reference,
                name,
                value,
            } => {
                let arguments = json!({
                    "variablesReference": reference,
                    "name": name,
                    "value": value,
                });
                match self.client.request("setVariable", arguments).await {
                    Ok(body) => {
                        self.state.variable_error = None;
                        let variables = self.variables(reference).await;
                        self.state.variables.insert(reference, variables);
                        // An adapter may answer with the value it stored; the tree reads
                        // the list again anyway, so the body is only a fallback.
                        let _ = body;
                    }
                    Err(error) => {
                        self.state.variable_error = Some(VariableError {
                            reference,
                            name,
                            message: error.to_string(),
                        });
                    }
                }
            }
            Command::Refresh => self.send_all_breakpoints().await,
            Command::Extension {
                command,
                arguments,
                reply,
            } => {
                let answer = self
                    .client
                    .request(&command, arguments)
                    .await
                    .map_err(|error| error.to_string());
                let _ = reply.send(answer);
            }
        }
        self.publish();
        Ok(false)
    }

    async fn disconnect(&mut self) {
        let _ = self
            .client
            .request("disconnect", json!({"terminateDebuggee": true}))
            .await;
        self.finish();
    }

    /// Sends a thread command; without a selected thread the adapter takes thread 0.
    fn thread_or_zero(&self) -> i64 {
        self.state.current_thread.unwrap_or(0)
    }

    async fn refresh_threads(&mut self) {
        if let Ok(body) = self.client.request("threads", json!({})).await {
            // The answer of a debuggee with ten thousand threads is large; it is parsed
            // off the thread that drives the session, and off the window in any case.
            let threads = tokio::task::spawn_blocking(move || threads_of(&body))
                .await
                .unwrap_or_default();
            self.state.threads = Arc::new(threads);
            if self.state.current_thread.is_none() {
                self.state.current_thread = self.state.threads.first().map(|thread| thread.id);
            }
        }
    }

    /// The stack of one thread. The window asks for it for the selected thread only;
    /// there is deliberately no request for every thread of the debuggee.
    async fn stack(&mut self, thread: i64) {
        let arguments = json!({"threadId": thread, "startFrame": 0, "levels": 40});
        let Ok(body) = self.client.request("stackTrace", arguments).await else {
            self.state.frames.clear();
            self.state.scopes.clear();
            return;
        };
        self.state.frames = frames_of(&body);
        let Some(first) = self.state.frames.first() else {
            self.state.scopes.clear();
            return;
        };
        self.state.selected_frame = Some(first.id);
        self.scopes(first.id).await;
    }

    async fn scopes(&mut self, frame: i64) {
        let Ok(body) = self
            .client
            .request("scopes", json!({"frameId": frame}))
            .await
        else {
            self.state.scopes.clear();
            return;
        };
        self.state.scopes = scopes_of(&body);
        let references: Vec<i64> = self
            .state
            .scopes
            .iter()
            .map(|scope| scope.reference)
            .collect();
        for reference in references {
            let variables = self.variables(reference).await;
            self.state.variables.insert(reference, variables);
        }
    }

    async fn variables(&self, reference: i64) -> Vec<Variable> {
        let arguments = json!({"variablesReference": reference, "start": 0, "count": 0});
        let Ok(body) = self.client.request("variables", arguments).await else {
            return Vec::new();
        };
        variables_of(&body)
    }

    async fn evaluate(&self, expression: &str, context: &str) -> Result<String, String> {
        let arguments = json!({
            "expression": expression,
            "frameId": self.state.selected_frame.unwrap_or(0),
            "context": context,
        });
        match self.client.request("evaluate", arguments).await {
            Ok(body) => Ok(body
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Evaluates for the popup at the caret, keeping what it takes to show the fields of
    /// an object.
    async fn evaluate_quick(&self, expression: &str) -> Evaluation {
        let arguments = json!({
            "expression": expression,
            "frameId": self.state.selected_frame.unwrap_or(0),
            "context": "repl",
        });
        match self.client.request("evaluate", arguments).await {
            Ok(body) => Evaluation {
                expression: expression.to_owned(),
                value: body
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                type_name: body
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                reference: body
                    .get("variablesReference")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                error: None,
            },
            Err(error) => Evaluation {
                expression: expression.to_owned(),
                error: Some(error.to_string()),
                ..Evaluation::default()
            },
        }
    }

    async fn evaluate_watch(&mut self) {
        let expressions: Vec<String> = self
            .state
            .watch
            .iter()
            .map(|w| w.expression.clone())
            .collect();
        for expression in expressions {
            let result = self.evaluate(&expression, "watch").await;
            if let Some(watch) = self
                .state
                .watch
                .iter_mut()
                .find(|watch| watch.expression == expression)
            {
                match result {
                    Ok(value) => {
                        watch.value = value;
                        watch.error = None;
                    }
                    Err(error) => {
                        watch.value.clear();
                        watch.error = Some(error);
                    }
                }
            }
        }
    }
}

fn threads_of(body: &Value) -> Vec<Thread> {
    body.get("threads")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    Some(Thread {
                        id: item.get("id")?.as_i64()?,
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn frames_of(body: &Value) -> Vec<Frame> {
    body.get("stackFrames")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|item| Frame {
                    id: item.get("id").and_then(Value::as_i64).unwrap_or(0),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    source: source_path(item.get("source")),
                    line: line_of(item),
                    column: item
                        .get("column")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        .max(0) as u32,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn step_targets_of(body: &Value) -> Vec<StepTarget> {
    body.get("targets")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    Some(StepTarget {
                        id: item.get("id")?.as_i64()?,
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn scopes_of(body: &Value) -> Vec<Scope> {
    body.get("scopes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|item| Scope {
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    reference: item
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    expensive: item
                        .get("expensive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn variables_of(body: &Value) -> Vec<Variable> {
    body.get("variables")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|item| Variable {
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    value: item
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    type_name: item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    reference: item
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The requests an adapter offers in its capabilities.
fn capabilities_of(capabilities: &Value) -> Capabilities {
    let flag = |key: &str| {
        capabilities
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Capabilities {
        function_breakpoints: flag("supportsFunctionBreakpoints"),
        data_breakpoints: flag("supportsDataBreakpoints"),
        restart_frame: flag("supportsRestartFrame"),
    }
}

/// The answers to `setFunctionBreakpoints` or `setDataBreakpoints`, which carry no place.
fn breakpoint_answers(body: &Value, kind: BreakpointKind) -> Vec<Breakpoint> {
    body.get("breakpoints")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|item| Breakpoint {
                    kind,
                    id: item.get("id").and_then(Value::as_i64),
                    verified: item
                        .get("verified")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    message: item
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    ..Breakpoint::default()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The exception filters an adapter offers in its capabilities.
fn filters_of(capabilities: &Value) -> Vec<ExceptionFilter> {
    capabilities
        .get("exceptionBreakpointFilters")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    let filter = item.get("filter")?.as_str()?.to_owned();
                    if filter.is_empty() {
                        return None;
                    }
                    Some(ExceptionFilter {
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or(&filter)
                            .to_owned(),
                        filter,
                        default: item
                            .get("default")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        description: item
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Keeps one entry per file and line, in a stable order.
pub fn normalise(breakpoints: Vec<(PathBuf, u32)>) -> Vec<(PathBuf, u32)> {
    let mut map: HashMap<PathBuf, BTreeSet<u32>> = HashMap::new();
    for (file, line) in breakpoints {
        map.entry(file).or_default().insert(line);
    }
    let mut all: Vec<(PathBuf, u32)> = map
        .into_iter()
        .flat_map(|(file, lines)| lines.into_iter().map(move |line| (file.clone(), line)))
        .collect();
    all.sort();
    all
}

/// Sending to a stream reader that is gone is not an error: the session has ended.
pub fn output_sender() -> (UnboundedSender<OutputLine>, UnboundedReceiver<OutputLine>) {
    tokio::sync::mpsc::unbounded_channel()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::DataAccessType;

    #[test]
    fn filters_come_from_the_capabilities() {
        let filters = filters_of(&json!({
            "exceptionBreakpointFilters": [
                {"filter": "caught", "label": "Caught Exceptions"},
                {"filter": "uncaught", "label": "Uncaught Exceptions", "default": true},
                {"label": "no id"}
            ]
        }));

        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].filter, "caught");
        assert!(filters[1].default);
        assert!(!filters[0].default);
    }

    #[test]
    fn the_capabilities_name_the_requests_of_methods_and_fields() {
        let capabilities = capabilities_of(&json!({
            "supportsFunctionBreakpoints": true,
            "supportsDataBreakpoints": true,
            "supportsRestartFrame": true
        }));
        assert_eq!(
            capabilities,
            Capabilities {
                function_breakpoints: true,
                data_breakpoints: true,
                restart_frame: true,
            }
        );
        assert_eq!(capabilities_of(&json!({})), Capabilities::default());
    }

    #[test]
    fn an_answer_about_a_method_or_field_breakpoint_keeps_its_id() {
        let answers = breakpoint_answers(
            &json!({
                "breakpoints": [
                    {"id": 7, "verified": true},
                    {"id": 8, "verified": false, "message": "no such field"},
                ]
            }),
            BreakpointKind::Data,
        );
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].id, Some(7));
        assert_eq!(answers[0].kind, BreakpointKind::Data);
        assert!(!answers[1].verified);
        assert_eq!(answers[1].message.as_deref(), Some("no such field"));
    }

    #[test]
    fn every_access_type_has_its_dap_name() {
        assert_eq!(DataAccessType::Read.as_dap(), "read");
        assert_eq!(DataAccessType::Write.as_dap(), "write");
        assert_eq!(DataAccessType::ReadWrite.as_dap(), "readWrite");
    }

    fn launch_at(address: String) -> Launch {
        Launch {
            address,
            adapter_id: "fixture".into(),
            name: "fixture".into(),
            function_breakpoints: vec![FunctionBreakpointSpec {
                name: "demo.Probe#work".into(),
                condition: Some("flag".into()),
                hit_condition: Some("2".into()),
                enabled: true,
            }],
            data_breakpoints: vec![DataBreakpointSpec {
                data_id: "demo.Probe#value".into(),
                access_type: DataAccessType::ReadWrite,
                condition: None,
                hit_condition: None,
                enabled: true,
            }],
            ..Launch::default()
        }
    }

    async fn wait_stopped(debugger: &crate::model::Debugger) -> Arc<Snapshot> {
        for _ in 0..400 {
            if let Some(snapshot) = debugger.poll()
                && matches!(snapshot.state, DebugState::Stopped { .. })
            {
                return snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the session did not stop");
    }

    /// The three requests of this kind reach the adapter, with the fields it reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn method_field_and_frame_requests_reach_the_adapter() {
        let server = lang_fixture::MockDap::start_with_breakpoint_kinds().unwrap();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let launch = launch_at(server.address().to_owned());
        let running = tokio::spawn(run(launch, receiver, snapshots));
        let debugger = crate::model::Debugger::new(commands, incoming);

        let stopped = wait_stopped(&debugger).await;
        assert!(stopped.capabilities.function_breakpoints);
        assert!(stopped.capabilities.data_breakpoints);
        assert!(stopped.capabilities.restart_frame);
        assert_eq!(stopped.function_breakpoints.len(), 1, "the answer is kept");

        debugger.send(Command::RestartFrame(12));
        let restarted = loop {
            if let Some(snapshot) = debugger.poll()
                && matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "restartframe")
            {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(
            restarted.state,
            DebugState::Stopped {
                reason: "restartframe".into()
            }
        );

        let requests = server.requests();
        let functions = requests
            .iter()
            .find(|request| request["command"] == "setFunctionBreakpoints")
            .expect("the method breakpoints were sent");
        assert_eq!(
            functions["arguments"]["breakpoints"][0]["name"],
            "demo.Probe#work"
        );
        assert_eq!(
            functions["arguments"]["breakpoints"][0]["condition"],
            "flag"
        );
        assert_eq!(
            functions["arguments"]["breakpoints"][0]["hitCondition"],
            "2"
        );
        let fields = requests
            .iter()
            .find(|request| request["command"] == "setDataBreakpoints")
            .expect("the field watchpoints were sent");
        assert_eq!(
            fields["arguments"]["breakpoints"][0]["dataId"],
            "demo.Probe#value"
        );
        assert_eq!(
            fields["arguments"]["breakpoints"][0]["accessType"],
            "readWrite"
        );
        let restart = requests
            .iter()
            .find(|request| request["command"] == "restartFrame")
            .expect("the frame restart was sent");
        assert_eq!(restart["arguments"]["frameId"], 12);

        debugger.send(Command::Disconnect);
        running.await.unwrap().unwrap();
        server.join();
    }
}
