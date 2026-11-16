//! A tiny ACP agent for tests: no network, no model. The prompt is the script:
//! `WRITE <path> <text>` lines write files, `ASK` asks for permission first, `HANG` waits for a
//! cancel, anything else is echoed back in pieces together with the names of linked files.

use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use serde_json::{Value, json};

struct Agent {
    inbox: Receiver<Value>,
    next_id: u64,
    cwd: String,
}

fn send(message: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn reply(id: &Value, result: Value) {
    send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn update(session: &str, update: Value) {
    let params = json!({"sessionId": session, "update": update});
    send(&json!({"jsonrpc": "2.0", "method": "session/update", "params": params}));
}

fn chunk(session: &str, text: &str) {
    let content = json!({"type": "text", "text": text});
    update(
        session,
        json!({"sessionUpdate": "agent_message_chunk", "content": content}),
    );
}

impl Agent {
    /// Waits for the answer to a request of ours; a cancel on the way counts as no.
    fn ask(&mut self, session: &str, call: &str, command: &str) -> bool {
        let id = self.next_id;
        self.next_id += 1;
        let options = json!([
            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
            {"optionId": "always", "name": "Always", "kind": "allow_always"},
            {"optionId": "no", "name": "Reject", "kind": "reject_once"},
        ]);
        let tool_call =
            json!({"toolCallId": call, "title": "Write files", "rawInput": {"command": command}});
        let params = json!({"sessionId": session, "toolCall": tool_call, "options": options});
        send(
            &json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission", "params": params}),
        );
        while let Ok(message) = self.inbox.recv() {
            if message["id"] == json!(id) && message.get("method").is_none() {
                let outcome = &message["result"]["outcome"];
                return outcome["optionId"] == json!("yes")
                    || outcome["optionId"] == json!("always");
            }
        }
        false
    }

    fn prompt(&mut self, id: &Value, params: &Value) {
        let session = params["sessionId"].as_str().unwrap_or_default().to_string();
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let mut text = String::new();
        let mut links = Vec::new();
        for block in &blocks {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                Some("resource_link") => {
                    links.push(block["name"].as_str().unwrap_or_default().to_string())
                }
                Some("resource") => links.push("resource".to_string()),
                _ => {}
            }
        }
        if text.contains("HANG") {
            while let Ok(message) = self.inbox.recv() {
                if message["method"] == json!("session/cancel") {
                    reply(id, json!({"stopReason": "cancelled"}));
                    return;
                }
            }
            return;
        }
        let writes: Vec<(String, String)> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("WRITE "))
            .filter_map(|rest| rest.split_once(' '))
            .map(|(path, body)| (path.to_string(), body.to_string()))
            .collect();
        if !writes.is_empty() {
            let call = "call-write";
            let title = format!("Write {} files", writes.len());
            update(
                &session,
                json!({"sessionUpdate": "tool_call", "toolCallId": call, "title": title, "kind": "edit", "status": "pending", "rawInput": {"files": writes.len()}}),
            );
            let paths: Vec<&str> = writes.iter().map(|(path, _)| path.as_str()).collect();
            if text.contains("ASK")
                && !self.ask(&session, call, &format!("write {}", paths.join(" ")))
            {
                update(
                    &session,
                    json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": "failed", "content": [{"type": "content", "content": {"type": "text", "text": "rejected by the user"}}]}),
                );
                chunk(&session, "I did not write anything.");
                reply(id, json!({"stopReason": "end_turn"}));
                return;
            }
            for (path, body) in &writes {
                let full = std::path::Path::new(&self.cwd).join(path);
                let _ = std::fs::write(full, format!("{body}\n"));
            }
            update(
                &session,
                json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "written"}}]}),
            );
            chunk(&session, "Done.");
            reply(id, json!({"stopReason": "end_turn"}));
            return;
        }
        chunk(&session, "echo: ");
        chunk(&session, &text);
        if !links.is_empty() {
            chunk(&session, &format!(" links: {}", links.join(",")));
        }
        reply(id, json!({"stopReason": "end_turn"}));
    }
}

fn main() {
    let (sender, inbox) = channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && sender.send(message).is_err()
            {
                return;
            }
        }
    });
    let mut agent = Agent {
        inbox,
        next_id: 1000,
        cwd: String::new(),
    };
    while let Ok(message) = agent.inbox.recv() {
        let id = message["id"].clone();
        let params = message["params"].clone();
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => reply(
                &id,
                json!({"protocolVersion": 1, "agentCapabilities": {"promptCapabilities": {"embeddedContext": true}}, "authMethods": []}),
            ),
            "session/new" => {
                agent.cwd = params["cwd"].as_str().unwrap_or(".").to_string();
                let servers = params["mcpServers"].as_array().map_or(0, Vec::len);
                eprintln!("mcp servers: {servers}");
                reply(&id, json!({"sessionId": "stub-session"}));
            }
            "session/prompt" => agent.prompt(&id, &params),
            _ => {}
        }
    }
}
