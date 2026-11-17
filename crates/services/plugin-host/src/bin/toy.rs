//! The toy plugin of the headless tests. `upper` upper-cases the selection through the
//! IDE, `hang` never answers, `crash` exits, `fail` answers with an error.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut next_id = 1000;
    let send = |message: Value| {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{message}").unwrap();
        stdout.flush().unwrap();
    };
    while let Some(Ok(line)) = lines.next() {
        let message: Value = serde_json::from_str(&line).unwrap();
        let id = message["id"].clone();
        match message["method"].as_str().unwrap_or("") {
            "initialize" => {
                let mcp = std::env::var("MCP_TOKEN").unwrap_or_default();
                let message = format!("started, MCP_TOKEN={mcp:?}");
                send(json!({"jsonrpc": "2.0", "method": "log", "params": {"message": message}}));
                send(json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            "command/execute" => match message["params"]["command"].as_str().unwrap_or("") {
                "upper" => {
                    next_id += 1;
                    send(
                        json!({"jsonrpc": "2.0", "id": next_id, "method": "editor/activeDocument"}),
                    );
                    let answer: Value = read_answer(&mut lines);
                    let text = answer["result"]["selectedText"]
                        .as_str()
                        .unwrap_or("")
                        .to_uppercase();
                    next_id += 1;
                    let params = json!({"text": text});
                    send(
                        json!({"jsonrpc": "2.0", "id": next_id, "method": "editor/replaceSelection", "params": params}),
                    );
                    read_answer(&mut lines);
                    send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
                }
                "hang" => {}
                "crash" => {
                    eprintln!("toy: asked to crash");
                    std::process::exit(3);
                }
                _ => {
                    let error = json!({"code": -32000, "message": "toy: this command fails"});
                    send(json!({"jsonrpc": "2.0", "id": id, "error": error}));
                }
            },
            "shutdown" => send(json!({"jsonrpc": "2.0", "id": id, "result": null})),
            "exit" => return,
            _ => {}
        }
    }
}

fn read_answer(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Value {
    let line = lines.next().unwrap().unwrap();
    serde_json::from_str(&line).unwrap()
}
