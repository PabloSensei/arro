//! A toy debug adapter for the tests of the client boundary.
//!
//! One scenario, the same on every connection: the debuggee stops on a breakpoint,
//! has two threads, a stack of two frames and variables with one expandable child,
//! takes steps, then continues and ends. The first frame deliberately has no source
//! and line zero, which a client must tolerate.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::{Value, json};

/// A running mock adapter.
pub struct MockDap {
    address: String,
    thread: Option<JoinHandle<()>>,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl MockDap {
    /// Binds a free port and starts serving. The first connection gets the scenario.
    pub fn start() -> std::io::Result<Self> {
        Self::start_with_threads(0, None, false)
    }

    /// The same scenario with `virtual_threads` extra threads, named the way the JDK
    /// names a virtual thread. A test reads a large thread list without a real JVM.
    pub fn start_with_virtual_threads(virtual_threads: usize) -> std::io::Result<Self> {
        Self::start_with_threads(virtual_threads, None, false)
    }

    /// The scenario with the stack frame of a real file, for the window actions that open
    /// the place of a frame.
    pub fn start_with_stack_source(source: impl Into<String>) -> std::io::Result<Self> {
        Self::start_with_threads(0, Some(source.into()), false)
    }

    /// The scenario of an adapter that lists the requests for method breakpoints, field
    /// watchpoints and frame restarts among its capabilities.
    pub fn start_with_breakpoint_kinds() -> std::io::Result<Self> {
        Self::start_with_threads(0, None, true)
    }

    /// The same, on a real file, for the window tests of the gutter.
    pub fn start_with_breakpoint_kinds_for(source: impl Into<String>) -> std::io::Result<Self> {
        Self::start_with_threads(0, Some(source.into()), true)
    }

    fn start_with_threads(
        virtual_threads: usize,
        source: Option<String>,
        breakpoint_kinds: bool,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?.to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let thread = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = serve(
                    stream,
                    &recorded,
                    virtual_threads,
                    source.as_deref(),
                    breakpoint_kinds,
                );
            }
        });
        Ok(Self {
            address,
            thread: Some(thread),
            seen,
        })
    }

    /// The requests the adapter received, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// `host:port` of the adapter.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Waits for the connection to end. A client that already disconnected must be
    /// joined by this, so a test leaves no thread behind.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MockDap {
    fn drop(&mut self) {
        // A dropped adapter leaves its thread waiting for the next request. Joining here
        // would hang a test that failed early; `join` is for the clean end of a scenario.
        drop(self.thread.take());
    }
}

fn serve(
    stream: TcpStream,
    seen: &Mutex<Vec<Value>>,
    virtual_threads: usize,
    source: Option<&str>,
    breakpoint_kinds: bool,
) -> std::io::Result<()> {
    let source = source.unwrap_or("C:/fixture/demo.sample");
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut seq = 0;
    let mut stopped = false;
    let mut exception_filters: Vec<String> = Vec::new();
    let mut exception_stopped = false;
    let mut text_value = "\"hello\"".to_owned();
    while let Some(message) = read_message(&mut reader)? {
        if message.get("type").and_then(Value::as_str) != Some("request") {
            continue;
        }
        seen.lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(message.clone());
        let command = message
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = message.get("arguments").cloned().unwrap_or(Value::Null);
        let request_seq = message.get("seq").and_then(Value::as_i64).unwrap_or(0);
        seq += 1;
        match command {
            "initialize" => {
                let mut body = json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsExceptionInfoRequest": true,
                    "supportsStepInTargetsRequest": true,
                    "supportsSetVariable": true,
                    "exceptionBreakpointFilters": [
                        {"filter": "caught", "label": "Caught Exceptions", "default": false},
                        {"filter": "uncaught", "label": "Uncaught Exceptions", "default": true}
                    ]
                });
                if breakpoint_kinds {
                    body["supportsFunctionBreakpoints"] = json!(true);
                    body["supportsDataBreakpoints"] = json!(true);
                    body["supportsRestartFrame"] = json!(true);
                }
                respond(&mut writer, seq, request_seq, command, body)?;
                seq += 1;
                event(&mut writer, seq, "initialized", json!({}))?;
            }
            "attach" => {
                respond(&mut writer, seq, request_seq, command, json!({}))?;
                seq += 1;
                event(
                    &mut writer,
                    seq,
                    "process",
                    json!({"name": "fixture", "systemProcessId": 4242}),
                )?;
            }
            "setBreakpoints" => {
                let breakpoints = arguments
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let entries: Vec<Value> = breakpoints
                    .iter()
                    .enumerate()
                    .map(|(index, breakpoint)| {
                        let condition = breakpoint
                            .get("condition")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if condition == "broken" {
                            // The adapter compiles a condition and reports a bad one here.
                            return json!({
                                "verified": false,
                                "line": breakpoint.get("line").cloned().unwrap_or(json!(0)),
                                "message": "Cannot compile condition: broken"
                            });
                        }
                        // The first answer hides the line, as a real adapter may do.
                        if index == 0 {
                            json!({"verified": true, "line": 0})
                        } else {
                            json!({"verified": true})
                        }
                    })
                    .collect();
                let entries = if entries.is_empty() {
                    vec![json!({"verified": true, "line": 0})]
                } else {
                    entries
                };
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({"breakpoints": entries}),
                )?;
            }
            "setFunctionBreakpoints" => {
                let breakpoints = arguments
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let entries: Vec<Value> = breakpoints
                    .iter()
                    .enumerate()
                    .map(|(index, breakpoint)| {
                        let name = breakpoint
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if name == "missing.Type#method" {
                            return json!({"verified": false, "message": "no such type"});
                        }
                        json!({"id": 300 + index as i64, "verified": true})
                    })
                    .collect();
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({"breakpoints": entries}),
                )?;
            }
            "dataBreakpointInfo" => {
                let name = arguments
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({
                        "dataId": format!("demo.Probe#{name}"),
                        "description": format!("Probe.{name} : int"),
                        "accessTypes": ["read", "write", "readWrite"],
                        "canPersist": true
                    }),
                )?;
            }
            "setDataBreakpoints" => {
                let breakpoints = arguments
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let entries: Vec<Value> = breakpoints
                    .iter()
                    .enumerate()
                    .map(|(index, breakpoint)| {
                        let data_id = breakpoint
                            .get("dataId")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if data_id == "demo.Probe#missing" {
                            return json!({"verified": false, "message": "no such field"});
                        }
                        json!({"id": 400 + index as i64, "verified": true})
                    })
                    .collect();
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({"breakpoints": entries}),
                )?;
            }
            "restartFrame" => {
                respond(&mut writer, seq, request_seq, command, json!({}))?;
                seq += 1;
                event(
                    &mut writer,
                    seq,
                    "stopped",
                    json!({
                        "reason": "restartframe",
                        "threadId": 1,
                        "allThreadsStopped": true,
                    }),
                )?;
            }
            "setExceptionBreakpoints" => {
                exception_filters = arguments
                    .get("filters")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                respond(&mut writer, seq, request_seq, command, json!({}))?;
            }
            "configurationDone" => {
                respond(&mut writer, seq, request_seq, command, json!({}))?;
                seq += 1;
                event(
                    &mut writer,
                    seq,
                    "output",
                    json!({"category": "stdout", "output": "hello from fixture\n"}),
                )?;
                seq += 1;
                event(
                    &mut writer,
                    seq,
                    "output",
                    json!({"category": "stderr", "output": "oops from fixture\n"}),
                )?;
                if !stopped {
                    stopped = true;
                    seq += 1;
                    event(
                        &mut writer,
                        seq,
                        "stopped",
                        json!({
                            "reason": "breakpoint",
                            "threadId": 1,
                            "allThreadsStopped": true,
                        }),
                    )?;
                }
            }
            "threads" => {
                let mut threads = vec![
                    json!({"id": 1, "name": "main"}),
                    json!({"id": 2, "name": "worker"}),
                ];
                for index in 0..virtual_threads {
                    let id = 1_000 + index as i64;
                    threads.push(json!({
                        "id": id,
                        "name": format!(
                            "VirtualThread[#{id}]/runnable@ForkJoinPool-1-worker-1"
                        ),
                    }));
                }
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({"threads": threads}),
                )?
            }
            "stackTrace" => respond(
                &mut writer,
                seq,
                request_seq,
                command,
                json!({"stackFrames": [
                    // No source and line zero on purpose.
                    {"id": 11, "name": "run", "source": {}, "line": 0, "column": 0},
                    {"id": 12, "name": "start", "source": {"path": source}, "line": 42, "column": 1},
                ]}),
            )?,
            "scopes" => respond(
                &mut writer,
                seq,
                request_seq,
                command,
                json!({"scopes": [
                    {"name": "Locals", "variablesReference": 100, "expensive": false},
                    {"name": "Statics", "variablesReference": 200, "expensive": true},
                ]}),
            )?,
            "variables" => {
                let reference = arguments
                    .get("variablesReference")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let body = match reference {
                    100 => json!({"variables": [
                        {"name": "text", "value": text_value, "type": "String", "variablesReference": 0},
                        {"name": "items", "value": "ArrayList (2)", "type": "ArrayList", "variablesReference": 101},
                    ]}),
                    101 => json!({"variables": [
                        {"name": "0", "value": "\"a\"", "type": "String", "variablesReference": 0},
                        {"name": "1", "value": "\"b\"", "type": "String", "variablesReference": 0},
                    ]}),
                    _ => json!({"variables": []}),
                };
                respond(&mut writer, seq, request_seq, command, body)?;
            }
            "evaluate" => {
                let expression = arguments
                    .get("expression")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                // A field of an object costs one more request, so a test can expand it.
                let body = if expression.contains("items") {
                    json!({"result": "ArrayList (2)", "type": "ArrayList", "variablesReference": 101})
                } else if expression.contains("text") {
                    json!({"result": text_value, "type": "String", "variablesReference": 0})
                } else {
                    json!({"result": "42", "type": "int", "variablesReference": 0})
                };
                respond(&mut writer, seq, request_seq, command, body)?
            }
            "setVariable" => {
                let name = arguments
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let value = arguments
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name == "items" {
                    // An adapter may refuse a field it cannot set.
                    respond_error(
                        &mut writer,
                        seq,
                        request_seq,
                        command,
                        "cannot set a field of ArrayList",
                    )?;
                    continue;
                }
                if name == "text" {
                    text_value = value.to_owned();
                }
                respond(
                    &mut writer,
                    seq,
                    request_seq,
                    command,
                    json!({"value": value}),
                )?
            }
            "stepInTargets" => respond(
                &mut writer,
                seq,
                request_seq,
                command,
                json!({"targets": [
                    {"id": 1, "label": "first()"},
                    {"id": 2, "label": "second()"},
                ]}),
            )?,
            "redefineClasses" => respond(
                &mut writer,
                seq,
                request_seq,
                command,
                json!({"changedClasses": ["demo.Probe", "demo.Probe$1"]}),
            )?,
            "continue" | "next" | "stepIn" | "stepOut" => {
                respond(&mut writer, seq, request_seq, command, json!({}))?;
                seq += 1;
                let exception = command == "continue"
                    && !exception_stopped
                    && exception_filters.iter().any(|filter| filter == "uncaught");
                if exception {
                    exception_stopped = true;
                }
                event(
                    &mut writer,
                    seq,
                    "stopped",
                    json!({
                        "reason": if exception {
                            "exception"
                        } else if command == "continue" {
                            "breakpoint"
                        } else {
                            "step"
                        },
                        "threadId": 1,
                        "allThreadsStopped": true,
                    }),
                )?;
            }
            "disconnect" => {
                respond(&mut writer, seq, request_seq, command, json!({}))?;
                seq += 1;
                event(&mut writer, seq, "terminated", json!({}))?;
                return Ok(());
            }
            _ => respond(&mut writer, seq, request_seq, command, json!({}))?,
        }
    }
    Ok(())
}

fn respond(
    writer: &mut TcpStream,
    seq: i64,
    request_seq: i64,
    command: &str,
    body: Value,
) -> std::io::Result<()> {
    write_message(
        writer,
        &json!({
            "seq": seq,
            "type": "response",
            "request_seq": request_seq,
            "success": true,
            "command": command,
            "body": body,
        }),
    )
}

fn respond_error(
    writer: &mut TcpStream,
    seq: i64,
    request_seq: i64,
    command: &str,
    message: &str,
) -> std::io::Result<()> {
    write_message(
        writer,
        &json!({
            "seq": seq,
            "type": "response",
            "request_seq": request_seq,
            "success": false,
            "command": command,
            "message": message,
        }),
    )
}

fn event(writer: &mut TcpStream, seq: i64, name: &str, body: Value) -> std::io::Result<()> {
    write_message(
        writer,
        &json!({"seq": seq, "type": "event", "event": name, "body": body}),
    )
}

fn write_message(writer: &mut TcpStream, message: &Value) -> std::io::Result<()> {
    let body = message.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    writer.flush()
}

fn read_message(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Value>> {
    let mut length = 0;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line
            .trim()
            .strip_prefix("Content-Length:")
            .and_then(|value| value.trim().parse().ok())
        {
            length = value;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scenario_answers_the_opening_requests() {
        let server = MockDap::start().unwrap();
        let stream = TcpStream::connect(server.address()).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        write_message(
            &mut writer,
            &json!({"seq": 1, "type": "request", "command": "initialize"}),
        )
        .unwrap();
        let response = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(response["request_seq"], 1);
        let event = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(event["event"], "initialized");

        write_message(
            &mut writer,
            &json!({"seq": 2, "type": "request", "command": "threads"}),
        )
        .unwrap();
        let response = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(response["request_seq"], 2);
        assert_eq!(response["success"], true);
        assert_eq!(response["body"]["threads"][0]["name"], "main");

        drop(writer);
        drop(reader);
        server.join();
    }
}
