//! The debugger tools that IDEA's server names `xdebug_*`, over plain HTTP: the names and
//! the parameters of the list, and the answers of a fake debug window that plays several
//! sessions, a stopped program and the points of an agent.

mod support;

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::json;
use support::Fixture;
use tool_gateway::{
    BreakpointOwner, DebugAnswer, DebugBreakpoint, DebugBreakpointQuery, DebugControlAction,
    DebugControlOutcome, DebugFrameInfo, DebugFramePage, DebugFrameValues, DebugQuery,
    DebugSetVariableOutcome, DebugThreadInfo, DebugThreadPage, DebugValueNode, DebuggerSession,
    DebuggerStatus, SessionScope, ToolBackend, ToolError,
};

/// A debug window of two possible sessions, a stopped program and the points an agent set.
struct Fake {
    /// How many sessions the window holds: one unless a test asks for two.
    sessions: usize,
    points: Mutex<Vec<DebugBreakpoint>>,
}

impl Fake {
    fn new(sessions: usize) -> Self {
        Self {
            sessions,
            points: Mutex::new(Vec::new()),
        }
    }

    fn status(&self) -> DebuggerStatus {
        let sessions: Vec<DebuggerSession> = (0..self.sessions)
            .map(|index| DebuggerSession {
                id: format!("debug-{}", index + 1),
                name: format!("Session {}", index + 1),
                status: "paused".to_string(),
                run_configuration: Some("Alpha".to_string()),
                ..DebuggerSession::default()
            })
            .collect();
        DebuggerStatus {
            active_session_id: sessions.first().map(|session| session.id.clone()),
            sessions,
        }
    }

    fn values() -> Vec<DebugValueNode> {
        vec![
            DebugValueNode {
                name: "text".to_string(),
                value: "\"hello\"".to_string(),
                type_name: "String".to_string(),
                children: Vec::new(),
            },
            DebugValueNode {
                name: "items".to_string(),
                value: "ArrayList (2)".to_string(),
                type_name: "ArrayList".to_string(),
                children: vec![DebugValueNode {
                    name: "0".to_string(),
                    value: "\"a\"".to_string(),
                    type_name: "String".to_string(),
                    children: vec![DebugValueNode {
                        name: "name".to_string(),
                        value: "\"first\"".to_string(),
                        type_name: "String".to_string(),
                        children: Vec::new(),
                    }],
                }],
            },
        ]
    }

    fn paused(session: Option<&String>) -> DebugControlOutcome {
        DebugControlOutcome {
            session_id: session.cloned().unwrap_or_else(|| "debug-1".to_string()),
            status: "paused".to_string(),
            paused: true,
            reason: Some("breakpoint".to_string()),
            frame_values: Vec::new(),
            breakpoint_errors_tail: vec!["src/Main.txt:9: the adapter did not accept".to_string()],
            tracepoint_outputs_tail: vec!["items = 2".to_string()],
        }
    }

    fn running(session: Option<&String>, status: &str) -> DebugControlOutcome {
        DebugControlOutcome {
            session_id: session.cloned().unwrap_or_else(|| "debug-1".to_string()),
            status: status.to_string(),
            ..DebugControlOutcome::default()
        }
    }
}

impl ToolBackend for Fake {
    fn editor_context(&self, _: &SessionScope) -> Result<tool_gateway::EditorContext, ToolError> {
        Ok(tool_gateway::EditorContext::default())
    }

    fn find_symbol(
        &self,
        _: &SessionScope,
        _: &tool_gateway::SymbolQuery,
    ) -> Result<tool_gateway::SymbolPage, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn symbol_info(
        &self,
        _: &SessionScope,
        _: &tool_gateway::Position,
    ) -> Result<tool_gateway::SymbolInfo, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        _: &tool_gateway::DiagnosticsQuery,
    ) -> Result<tool_gateway::DiagnosticsPage, ToolError> {
        Err(ToolError::Failed("not here".to_string()))
    }

    fn run_points(
        &self,
        _: &SessionScope,
        query: &tool_gateway::RunPointQuery,
    ) -> Result<Vec<tool_gateway::RunPointInfo>, ToolError> {
        Ok(vec![tool_gateway::RunPointInfo {
            kind: "program".to_string(),
            name: "main".to_string(),
            container: "demo.Main".to_string(),
            file: query.file_path.clone(),
            line: 1,
            framework: false,
        }])
    }

    fn run_configurations(
        &self,
        _: &SessionScope,
    ) -> Result<Vec<tool_gateway::RunConfiguration>, ToolError> {
        Ok(vec![tool_gateway::RunConfiguration {
            name: "Main".to_string(),
            kind: "program".to_string(),
            command_line: None,
            working_directory: None,
            environment: Vec::new(),
            supports_dynamic_launch_overrides: true,
        }])
    }

    fn debug_sessions(
        &self,
        _scope: &SessionScope,
        query: &DebugQuery,
    ) -> Result<DebugAnswer, ToolError> {
        match query {
            DebugQuery::Status => Ok(DebugAnswer::Status(self.status())),
            DebugQuery::Start(query) => {
                let name = query
                    .configuration_name
                    .clone()
                    .ok_or_else(|| ToolError::BadInput("no configuration".to_string()))?;
                Ok(DebugAnswer::Started(DebuggerSession {
                    id: "debug-9".to_string(),
                    name: name.clone(),
                    status: "starting".to_string(),
                    run_configuration: Some(name),
                    ..DebuggerSession::default()
                }))
            }
            DebugQuery::Control(query) => match query.action {
                DebugControlAction::WaitForPause | DebugControlAction::StepInto => Ok(
                    DebugAnswer::Controlled(Self::paused(query.session_id.as_ref())),
                ),
                DebugControlAction::Stop => Ok(DebugAnswer::Controlled(Self::running(
                    query.session_id.as_ref(),
                    "stopped",
                ))),
                _ => Ok(DebugAnswer::Controlled(Self::running(
                    query.session_id.as_ref(),
                    "running",
                ))),
            },
            DebugQuery::RunToLine(query) => Ok(DebugAnswer::Controlled(Self::paused(
                query.session_id.as_ref(),
            ))),
            DebugQuery::Threads(_) => Ok(DebugAnswer::Threads(DebugThreadPage {
                threads: vec![DebugThreadInfo {
                    id: 1,
                    name: "main".to_string(),
                    state: "paused".to_string(),
                    is_current: true,
                    additional_info: None,
                    additional_info_tooltip: None,
                    frame_count: 1,
                }],
                total: 1,
            })),
            DebugQuery::Frames(_) => Ok(DebugAnswer::Frames(DebugFramePage {
                thread_id: Some(1),
                thread_name: Some("main".to_string()),
                frames: vec![DebugFrameInfo {
                    index: 0,
                    file: Some(PathBuf::from("src/Main.txt")),
                    line: 12,
                    is_current: true,
                    presentation: "run".to_string(),
                }],
                total: 1,
            })),
            DebugQuery::Values(query) => Ok(DebugAnswer::Values(DebugFrameValues {
                frame_index: query.frame_index,
                values: Self::values(),
            })),
            DebugQuery::Evaluate(query) => Ok(DebugAnswer::Value(DebugValueNode {
                name: query.expression.clone(),
                value: "2".to_string(),
                type_name: "int".to_string(),
                children: Vec::new(),
            })),
            DebugQuery::SetVariable(query) => Ok(DebugAnswer::Set(DebugSetVariableOutcome {
                old_value: Some("\"hello\"".to_string()),
                new_value: query.new_value.clone(),
                applied: true,
            })),
            DebugQuery::Breakpoints(_) => Ok(DebugAnswer::Breakpoints(
                self.points.lock().unwrap().clone(),
            )),
            DebugQuery::SetBreakpoint(query) => {
                let point = point_of(query);
                let mut points = self.points.lock().unwrap();
                points.retain(|known| known.id != point.id);
                points.push(point.clone());
                Ok(DebugAnswer::Breakpoint(point))
            }
            DebugQuery::RemoveBreakpoints(query) => {
                let mut points = self.points.lock().unwrap();
                let before = points.len();
                points.retain(|point| {
                    let owner = query.owner.is_none_or(|owner| point.owner == owner);
                    let id = query
                        .breakpoint_id
                        .as_ref()
                        .is_none_or(|id| point.id == *id);
                    let file = query
                        .file_path
                        .as_ref()
                        .is_none_or(|file| point.file == *file);
                    let line = query.line.is_none_or(|line| point.line == line);
                    !(owner && id && file && line)
                });
                Ok(DebugAnswer::Removed(before - points.len()))
            }
        }
    }
}

fn point_of(query: &DebugBreakpointQuery) -> DebugBreakpoint {
    let file = query.file_path.clone().unwrap_or_default();
    let line = query.line.unwrap_or(0);
    DebugBreakpoint {
        id: tool_gateway::breakpoint_id(&file, line),
        kind: "LINE".to_string(),
        file,
        line,
        enabled: query.enabled.unwrap_or(true),
        verified: true,
        owner: query.owner.unwrap_or(BreakpointOwner::Agent),
        condition: query.condition.clone().flatten(),
        is_log_message: query.is_log_message.unwrap_or(false),
        is_log_stack: query.is_log_stack.unwrap_or(false),
        temporary: query.temporary.unwrap_or(false),
        suspend_policy: query
            .suspend_policy
            .clone()
            .unwrap_or_else(|| "ALL".to_string()),
        hit_count: query.hit_count.unwrap_or(0),
        line_text: Some("run();".to_string()),
    }
}

fn fixture(sessions: usize) -> Fixture {
    Fixture::start(std::sync::Arc::new(Fake::new(sessions)))
}

#[test]
fn the_list_names_every_xdebug_tool_with_its_parameters() {
    let fixture = fixture(1);

    let list = fixture.list();

    for (name, parameter) in [
        ("xdebug_get_debugger_status", "sessions"),
        ("xdebug_start_debugger_session", "configurationName"),
        ("xdebug_start_debugger_session", "graceWaitMs"),
        ("xdebug_control_session", "action"),
        ("xdebug_control_session", "eventsLimit"),
        ("xdebug_run_to_line", "filePath"),
        ("xdebug_get_threads", "offset"),
        ("xdebug_get_stack", "threadId"),
        ("xdebug_get_frame_values", "frameIndex"),
        ("xdebug_get_value_by_path", "path"),
        ("xdebug_evaluate_expression", "depth"),
        ("xdebug_set_variable", "newValue"),
        ("xdebug_list_breakpoints", "filePath"),
        ("xdebug_set_breakpoint", "suspendPolicy"),
        ("xdebug_set_breakpoint", "hitCount"),
        ("xdebug_remove_breakpoint", "owner"),
    ] {
        assert!(list.contains(name), "{name} is not listed");
        let at = list.find(name).expect(name);
        assert!(
            list[at..].contains(parameter),
            "{name} does not name {parameter}"
        );
    }
    assert!(
        list.contains("sessionId"),
        "the session parameter is missing"
    );
}

#[test]
fn every_xdebug_tool_answers() {
    let fixture = fixture(1);
    let calls = [
        ("xdebug_get_debugger_status", json!({})),
        (
            "xdebug_start_debugger_session",
            json!({"configurationName": "Alpha", "timeout": 1000}),
        ),
        (
            "xdebug_control_session",
            json!({"action": "WAIT_FOR_PAUSE", "timeout": 1000}),
        ),
        (
            "xdebug_run_to_line",
            json!({"filePath": "src/Main.txt", "line": 12, "timeout": 1000}),
        ),
        ("xdebug_get_threads", json!({"limit": 10})),
        ("xdebug_get_stack", json!({"limit": 10})),
        ("xdebug_get_frame_values", json!({"depth": 1})),
        (
            "xdebug_get_value_by_path",
            json!({"path": ["items", "[0]"]}),
        ),
        (
            "xdebug_evaluate_expression",
            json!({"expression": "items.size()", "depth": 1}),
        ),
        (
            "xdebug_set_variable",
            json!({"path": ["text"], "newValue": "\"bye\""}),
        ),
        ("xdebug_list_breakpoints", json!({})),
        (
            "xdebug_set_breakpoint",
            json!({"filePath": "src/Main.txt", "line": 12, "condition": "x > 1"}),
        ),
        ("xdebug_remove_breakpoint", json!({"owner": "agent"})),
    ];
    for (name, arguments) in calls {
        let (answer, error) = fixture.answer(name, arguments);
        assert!(!error, "{name}: {answer}");
    }
}

#[test]
fn an_agent_point_is_listed_with_its_owner_and_removed_by_owner() {
    let fixture = fixture(1);

    let (set, error) = fixture.answer(
        "xdebug_set_breakpoint",
        json!({
            "filePath": "src/Main.txt",
            "line": 12,
            "condition": "x > 1",
            "temporary": true,
            "hitCount": 3,
        }),
    );
    assert!(!error, "{set}");
    assert!(set.contains("\"owner\": \"agent\""), "{set}");
    assert!(set.contains("\"condition\": \"x > 1\""), "{set}");
    assert!(set.contains("\"temporary\": true"), "{set}");
    assert!(set.contains("\"hitCount\": 3"), "{set}");

    let (listed, error) = fixture.answer("xdebug_list_breakpoints", json!({}));
    assert!(!error, "{listed}");
    assert!(listed.contains("\"owner\": \"agent\""), "{listed}");
    assert!(listed.contains("src/Main.txt:12"), "{listed}");

    let journal = fixture.gateway.clients().all_calls();
    assert!(
        journal.iter().any(
            |call| call.tool == "xdebug_set_breakpoint" && call.arguments.contains("Main.txt")
        ),
        "{journal:?}"
    );

    let (removed, error) = fixture.answer("xdebug_remove_breakpoint", json!({"owner": "agent"}));
    assert!(!error, "{removed}");
    assert!(removed.contains("\"removed\": 1"), "{removed}");
    let (empty, _) = fixture.answer("xdebug_list_breakpoints", json!({}));
    assert!(empty.contains("\"breakpoints\": []"), "{empty}");
}

#[test]
fn a_wait_for_pause_answers_the_paused_program_and_its_values() {
    let fixture = fixture(1);

    let (answer, error) = fixture.answer(
        "xdebug_control_session",
        json!({"action": "WAIT_FOR_PAUSE", "timeout": 1000}),
    );

    assert!(!error, "{answer}");
    assert!(answer.contains("\"paused\": true"), "{answer}");
    assert!(answer.contains("\"status\": \"paused\""), "{answer}");
    assert!(answer.contains("\"frameValues\""), "{answer}");
    assert!(answer.contains("\"name\": \"items\""), "{answer}");
    assert!(answer.contains("\"tracepointOutputsTail\""), "{answer}");
}

#[test]
fn a_value_is_read_by_its_path_through_an_item() {
    let fixture = fixture(1);

    let (answer, error) = fixture.answer(
        "xdebug_get_value_by_path",
        json!({"path": ["items", "[0]", "name"], "depth": 0}),
    );

    assert!(!error, "{answer}");
    assert!(answer.contains("\"items\""), "{answer}");
    assert!(answer.contains("\"name\": \"name\""), "{answer}");
    assert!(answer.contains("\"value\": \"\\\"first\\\"\""), "{answer}");

    let (missing, error) = fixture.answer(
        "xdebug_get_value_by_path",
        json!({"path": ["items", "[9]"]}),
    );
    assert!(error, "{missing}");
    assert!(missing.contains("has no value at items[9]"), "{missing}");
}

#[test]
fn a_temporary_session_starts_from_a_place_of_a_file() {
    let fixture = fixture(1);

    let (started, error) = fixture.answer(
        "xdebug_start_debugger_session",
        json!({"filePath": "src/Main.txt", "line": 2, "graceWaitMs": 1000}),
    );

    assert!(!error, "{started}");
    assert!(
        started.contains("\"runConfigurationName\": \"Main\""),
        "{started}"
    );
    assert!(started.contains("\"sessionId\""), "{started}");
}

#[test]
fn two_sessions_need_a_session_id_and_one_is_chosen_alone() {
    let many = fixture(2);

    let (answer, error) = many.answer("xdebug_get_threads", json!({}));
    assert!(error, "{answer}");
    assert!(answer.contains("pass sessionId"), "{answer}");

    let (chosen, error) = many.answer(
        "xdebug_get_threads",
        json!({"sessionId": "debug-2", "limit": 10}),
    );
    assert!(!error, "{chosen}");
    assert!(chosen.contains("\"name\": \"main\""), "{chosen}");

    let single = fixture(1);
    let (answer, error) = single.answer("xdebug_get_threads", json!({}));
    assert!(!error, "{answer}");
    assert!(answer.contains("\"name\": \"main\""), "{answer}");
}
