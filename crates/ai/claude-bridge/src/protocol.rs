//! The MCP methods on the other end of the WebSocket: the four tools of the IDE protocol
//! and the two that close a diff. The shapes follow the harness: diagnostics are one
//! JSON document in a text block, `openDiff` answers `FILE_SAVED` with the final text,
//! `DIFF_REJECTED` or `TAB_CLOSED`.

use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use platform::path_from_uri;
use serde_json::{Value, json};

use crate::{FileDiagnostics, Shared, write_message};

pub fn file_uri(path: &Path) -> String {
    let shown = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !shown.starts_with('/') {
        uri.push('/');
    }
    for byte in shown.bytes() {
        let keep =
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':');
        if keep {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn text_block(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn diagnostics_file(file: &FileDiagnostics) -> Value {
    let items: Vec<Value> = file
        .diagnostics
        .iter()
        .map(|item| {
            json!({
                "message": item.message,
                "severity": item.severity.name(),
                "range": {
                    "start": {"line": item.line, "character": item.character},
                    "end": {"line": item.line, "character": item.character + 1},
                },
                "source": item.source,
                "code": item.code,
            })
        })
        .collect();
    json!({"uri": file_uri(&file.path), "diagnostics": items})
}

fn get_diagnostics(shared: &Shared, arguments: &Value) -> Value {
    let files: Vec<Value> = match arguments.get("uri").and_then(Value::as_str) {
        Some(uri) => match path_from_uri(uri) {
            Some(path) => {
                let found = shared.diagnostics().files(Some(&path));
                if found.is_empty() {
                    vec![json!({"uri": file_uri(&path), "diagnostics": []})]
                } else {
                    found.iter().map(diagnostics_file).collect()
                }
            }
            None => vec![json!({"uri": uri, "diagnostics": []})],
        },
        None => shared
            .diagnostics()
            .files(None)
            .iter()
            .map(diagnostics_file)
            .collect(),
    };
    text_block(serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string()))
}

fn open_editors(shared: &Shared) -> Value {
    let editors: Vec<Value> = shared
        .editors()
        .editors()
        .iter()
        .map(|editor| json!({"uri": file_uri(&editor.path), "active": editor.active}))
        .collect();
    text_block(serde_json::to_string(&editors).unwrap_or_else(|_| "[]".to_string()))
}

fn current_selection(shared: &Shared) -> Value {
    let text = match shared.editors().selection() {
        Some(selection) => json!({
            "uri": file_uri(&selection.path),
            "selection": {
                "start": {"line": selection.start_line, "character": selection.start_character},
                "end": {"line": selection.end_line, "character": selection.end_character},
            },
            "text": selection.text,
        })
        .to_string(),
        None => "null".to_string(),
    };
    text_block(text)
}

fn open_diff(
    shared: &Shared,
    arguments: &Value,
    id: Value,
    writer: &Arc<Mutex<TcpStream>>,
    connection: u64,
) -> Result<Option<Value>, String> {
    let old = arguments
        .get("old_file_path")
        .and_then(Value::as_str)
        .ok_or("old_file_path is missing")?;
    let new = arguments
        .get("new_file_path")
        .and_then(Value::as_str)
        .unwrap_or(old);
    let proposed = arguments
        .get("new_file_contents")
        .and_then(Value::as_str)
        .ok_or("new_file_contents is missing")?;
    let tab_name = arguments
        .get("tab_name")
        .and_then(Value::as_str)
        .unwrap_or("Claude Code");
    let path = crate::resolve(shared.project(), Path::new(new))
        .ok_or("the file is outside the project")?;
    let old_path = crate::resolve(shared.project(), Path::new(old))
        .ok_or("the old file is outside the project")?;
    let base = match fs::read(&old_path) {
        Ok(bytes) => Some(
            String::from_utf8(bytes)
                .map_err(|_| "the old file is not UTF-8, a diff cannot show it".to_string())?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("the old file is not read: {error}")),
    };
    shared.propose(
        path,
        tab_name.to_string(),
        base,
        proposed.to_string(),
        id,
        writer.clone(),
        connection,
    );
    Ok(None)
}

fn close_tab(shared: &Shared, arguments: &Value, connection: u64) -> Value {
    if let Some(name) = arguments.get("tab_name").and_then(Value::as_str) {
        for id in shared.pending_on(connection, Some(name)) {
            if let Some(pending) = shared.close(id) {
                write_message(
                    &pending.writer,
                    &result(pending.request, text_block("TAB_CLOSED".to_string())),
                );
            }
        }
    }
    text_block("TAB_CLOSED".to_string())
}

fn close_all(shared: &Shared, connection: u64) -> Value {
    for id in shared.pending_on(connection, None) {
        if let Some(pending) = shared.close(id) {
            write_message(
                &pending.writer,
                &result(pending.request, text_block("TAB_CLOSED".to_string())),
            );
        }
    }
    text_block("TAB_CLOSED".to_string())
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

fn tools() -> Value {
    json!({"tools": [
        tool(
            "getDiagnostics",
            "Errors and warnings of the project that the IDE shows in Problems, as \
             `path:line:message`. One file by its `file:` URI, or the whole project.",
            json!({
                "type": "object",
                "properties": {
                    "uri": {"type": "string", "description": "A `file:` URI of one file"},
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "openDiff",
            "Shows a proposed text of a file in the editor as a review with accept and \
             reject by hunk. The answer is FILE_SAVED with the text the user kept, \
             DIFF_REJECTED or TAB_CLOSED. The file changes on disk only after the user \
             decides.",
            json!({
                "type": "object",
                "properties": {
                    "old_file_path": {"type": "string", "description": "The file as it is on disk"},
                    "new_file_path": {"type": "string", "description": "The file to write"},
                    "new_file_contents": {"type": "string", "description": "The proposed text"},
                    "tab_name": {"type": "string", "description": "The name of the review tab"},
                },
                "required": ["old_file_path", "new_file_path", "new_file_contents"],
                "additionalProperties": false,
            }),
        ),
        tool(
            "getOpenEditors",
            "The files open in the editor, as `file:` URIs, and which one is active.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        tool(
            "getCurrentSelection",
            "The selection of the active editor: its file, its range and its text.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        tool(
            "close_tab",
            "Closes the tab of a review the IDE shows.",
            json!({
                "type": "object",
                "properties": {"tab_name": {"type": "string"}},
                "required": ["tab_name"],
                "additionalProperties": false,
            }),
        ),
        tool(
            "closeAllDiffTabs",
            "Closes every review tab of the IDE.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
    ]})
}

fn call_tool(
    shared: &Shared,
    arguments: &Value,
    id: Value,
    writer: &Arc<Mutex<TcpStream>>,
    connection: u64,
) -> Result<Option<Value>, String> {
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .ok_or("the tool name is missing")?;
    let input = arguments
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let answer = match name {
        "getDiagnostics" => Some(result(id, get_diagnostics(shared, &input))),
        "getOpenEditors" => Some(result(id, open_editors(shared))),
        "getCurrentSelection" => Some(result(id, current_selection(shared))),
        "openDiff" => return open_diff(shared, &input, id, writer, connection),
        "close_tab" => Some(result(id, close_tab(shared, &input, connection))),
        "closeAllDiffTabs" => Some(result(id, close_all(shared, connection))),
        other => return Err(format!("the IDE has no tool {other}")),
    };
    Ok(answer)
}

/// The answer to one message; `None` for a notification and for a diff that waits for
/// the user. Unknown notifications are ignored, as the protocol asks.
pub(crate) fn answer(
    shared: &Shared,
    text: &str,
    writer: &Arc<Mutex<TcpStream>>,
    connection: u64,
) -> Option<Value> {
    let message: Value = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(_) => {
            return Some(error(Value::Null, -32700, "the message is not JSON"));
        }
    };
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    let method = method?;
    let id = id?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            Some(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "arro-ide", "version": env!("CARGO_PKG_VERSION")},
                }),
            ))
        }
        "ping" => Some(result(id, json!({}))),
        "tools/list" => Some(result(id, tools())),
        "tools/call" => match call_tool(shared, &params, id.clone(), writer, connection) {
            Ok(answer) => answer,
            Err(text) => Some(error(id, -32602, &text)),
        },
        other => Some(error(id, -32601, &format!("the IDE has no method {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    #[cfg(windows)]
    #[test]
    fn uris_round_trip_with_spaces_and_drives() {
        let path = Path::new("C:/work/a b/File.txt");
        let uri = file_uri(path);
        assert_eq!(uri, "file:///C:/work/a%20b/File.txt");
        assert_eq!(path_from_uri(&uri), Some(path.to_path_buf()));
    }

    #[cfg(not(windows))]
    #[test]
    fn uris_round_trip_with_spaces() {
        let path = Path::new("/work/a b/File.txt");
        let uri = file_uri(path);
        assert_eq!(uri, "file:///work/a%20b/File.txt");
        assert_eq!(path_from_uri(&uri), Some(path.to_path_buf()));
    }

    #[test]
    fn a_diagnostic_carries_its_range_and_severity() {
        let file = FileDiagnostics {
            path: PathBuf::from("C:/work/a.txt"),
            diagnostics: vec![crate::Diagnostic {
                line: 2,
                character: 4,
                severity: crate::Severity::Error,
                message: "broken".to_string(),
                source: Some("ide".to_string()),
                code: Some("E1".to_string()),
            }],
        };

        let value = diagnostics_file(&file);

        assert_eq!(value["diagnostics"][0]["severity"], json!("Error"));
        assert_eq!(value["diagnostics"][0]["range"]["start"]["line"], json!(2));
        assert_eq!(
            value["diagnostics"][0]["range"]["start"]["character"],
            json!(4)
        );
        assert_eq!(
            value["diagnostics"][0]["range"]["end"]["character"],
            json!(5)
        );
    }

    #[test]
    fn the_tool_list_names_the_protocol_tools() {
        let names: Vec<String> = tools()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();

        for name in [
            "getDiagnostics",
            "openDiff",
            "getOpenEditors",
            "getCurrentSelection",
            "close_tab",
            "closeAllDiffTabs",
        ] {
            assert!(names.contains(&name.to_string()), "{name}");
        }
    }
}
