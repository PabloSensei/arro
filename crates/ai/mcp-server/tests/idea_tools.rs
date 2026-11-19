//! The tools that IDEA names, over plain HTTP: the names and the parameters of the list,
//! and the answer of a call on a small project on disk.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mcp_server::McpServer;
use serde_json::{Value, json};
use test_support::TempDir;
use tool_gateway::{
    AnalyzeCallsQuery, Approval, ApprovalAnswer, ApprovalRequest, AuditLog, CallNode, CallTree,
    Confidence, DiagnosticsPage, DiagnosticsQuery, EditorContext, FileActionOutcome,
    FileActionQuery, FileContent, FileText, GitFileEntry, GitRepositoryStatus, GitStatus,
    GitStatusQuery, PathPage, PathQuery, Position, ProposeEditQuery, ProposedEdit, ReadFileQuery,
    RenameSymbolQuery, RenamedSymbol, RunConfiguration, RunOutput, RunPointInfo, RunPointQuery,
    SearchHit, SearchPage, SearchQuery, SessionScope, Severity, StartRunRequest, SymbolHit,
    SymbolInfo, SymbolPage, SymbolQuery, TerminalOutput, TerminalQuery, ToolBackend, ToolError,
    ToolGateway,
};

struct Allow;

impl Approval for Allow {
    fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Once
    }
}

#[derive(Default)]
struct Fake {
    root: PathBuf,
    opened: Mutex<Vec<PathBuf>>,
    proposed: Mutex<Option<(PathBuf, String)>>,
}

impl Fake {
    fn file(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
}

impl ToolBackend for Fake {
    fn editor_context(&self, _: &SessionScope) -> Result<EditorContext, ToolError> {
        Ok(EditorContext {
            file: Some(self.file("src/Main.txt")),
            line: Some(1),
            column: Some(1),
            selection: None,
            open_files: vec![self.file("src/Main.txt"), self.file("src/Other.txt")],
        })
    }

    fn find_symbol(&self, _: &SessionScope, query: &SymbolQuery) -> Result<SymbolPage, ToolError> {
        Ok(SymbolPage {
            hits: vec![SymbolHit {
                name: query.query.clone(),
                kind: "class".to_string(),
                path: self.file("src/Main.txt"),
                line: 1,
                content: format!("class {}", query.query),
                before: Vec::new(),
                after: Vec::new(),
            }],
            source: "fake".to_string(),
            confidence: Confidence::Certain,
            next: None,
        })
    }

    fn symbol_info(&self, _: &SessionScope, position: &Position) -> Result<SymbolInfo, ToolError> {
        Ok(SymbolInfo {
            name: "Widget".to_string(),
            kind: "class".to_string(),
            signature: Some(format!("class Widget at line {}", position.line)),
            documentation: None,
            declaration: None,
            source: "fake".to_string(),
            confidence: Confidence::Certain,
        })
    }

    fn diagnostics(
        &self,
        _: &SessionScope,
        _: &DiagnosticsQuery,
    ) -> Result<DiagnosticsPage, ToolError> {
        Ok(DiagnosticsPage {
            items: vec![tool_gateway::Diagnostic {
                path: self.file("src/Main.txt"),
                line: 2,
                column: 3,
                severity: Severity::Error,
                message: "cannot resolve".to_string(),
                code: Some("E1".to_string()),
                content: "bad();".to_string(),
            }],
            source: "fake".to_string(),
            confidence: Confidence::Certain,
            next: None,
        })
    }

    fn file_text(&self, _: &SessionScope, path: &Path) -> Result<FileContent, ToolError> {
        let text =
            std::fs::read_to_string(path).map_err(|error| ToolError::Failed(error.to_string()))?;
        Ok(FileContent {
            path: path.to_path_buf(),
            text,
            dirty: false,
        })
    }

    fn read_file(&self, _: &SessionScope, query: &ReadFileQuery) -> Result<FileText, ToolError> {
        let text = std::fs::read_to_string(&query.path)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        let start = query.start_line.unwrap_or(1);
        let end = query.end_line.unwrap_or(start);
        Ok(FileText {
            path: query.path.clone(),
            text,
            start_line: start,
            end_line: end,
            total_lines: end - start + 1,
            dirty: false,
            clipped: false,
        })
    }

    fn find_files(&self, _: &SessionScope, query: &PathQuery) -> Result<PathPage, ToolError> {
        let all = [
            "src/Main.txt",
            "src/Other.txt",
            "src/generated/Gen.txt",
            "docs/notes.md",
        ];
        let mut items: Vec<PathBuf> = all
            .iter()
            .filter(|path| {
                tool_gateway::path_allowed(&query.paths, path)
                    && tool_gateway::matches_glob(&query.pattern, path)
            })
            .map(|path| self.file(path))
            .collect();
        let total = items.len();
        items.truncate(query.limit);
        Ok(PathPage {
            items,
            total,
            clipped: total > query.limit,
        })
    }

    fn search_text(&self, _: &SessionScope, _: &SearchQuery) -> Result<SearchPage, ToolError> {
        Ok(SearchPage {
            items: vec![SearchHit {
                path: self.file("src/Main.txt"),
                line: 2,
                column: 1,
                content: "bad();".to_string(),
            }],
            total: 1,
            clipped: false,
            files: 1,
        })
    }

    fn analyze_calls(
        &self,
        _: &SessionScope,
        query: &AnalyzeCallsQuery,
    ) -> Result<CallTree, ToolError> {
        Ok(CallTree {
            symbol: query.symbol_fqn.clone(),
            direction: query.analysis_kind,
            root: Some(CallNode {
                name: "run".to_string(),
                detail: None,
                file_path: Some(self.file("src/Main.txt")),
                line: 1,
                tree_path: vec![],
                children: vec![CallNode {
                    name: "caller".to_string(),
                    detail: None,
                    file_path: Some(self.file("src/Other.txt")),
                    line: 3,
                    tree_path: vec![0],
                    children: Vec::new(),
                }],
            }),
            more: false,
        })
    }

    fn project_structure(
        &self,
        _: &SessionScope,
        _: &tool_gateway::ProjectStructureQuery,
    ) -> Result<tool_gateway::ProjectStructure, ToolError> {
        Ok(tool_gateway::ProjectStructure {
            modules: vec![tool_gateway::ProjectModule {
                name: "app".to_string(),
                path: PathBuf::from("."),
                source_roots: vec![self.file("src")],
                test_roots: Vec::new(),
                dependencies: vec!["group:lib:1".to_string()],
                dependencies_clipped: false,
            }],
            total_modules: 1,
            clipped: false,
            build_system: "fake".to_string(),
            build_system_version: None,
            resolved: true,
            runtime: Some("21".to_string()),
            language_level: Some("21".to_string()),
            problems: Vec::new(),
        })
    }

    fn run_points(
        &self,
        _: &SessionScope,
        _: &RunPointQuery,
    ) -> Result<Vec<RunPointInfo>, ToolError> {
        Ok(vec![
            RunPointInfo {
                kind: "program".to_string(),
                name: "demo.Main".to_string(),
                container: "demo.Main".to_string(),
                file: self.file("src/Main.txt"),
                line: 1,
                framework: false,
            },
            RunPointInfo {
                kind: "test".to_string(),
                name: "demo.MainTest".to_string(),
                container: "demo.MainTest".to_string(),
                file: self.file("src/Main.txt"),
                line: 8,
                framework: false,
            },
        ])
    }

    fn run_configurations(&self, _: &SessionScope) -> Result<Vec<RunConfiguration>, ToolError> {
        Ok(vec![RunConfiguration {
            name: "Main".to_string(),
            kind: "program".to_string(),
            command_line: Some("demo.Main".to_string()),
            working_directory: None,
            environment: vec![("MODE".to_string(), "test".to_string())],
            supports_dynamic_launch_overrides: true,
        }])
    }

    fn start_run(
        &self,
        _: &SessionScope,
        _: &StartRunRequest,
        _: Option<&str>,
    ) -> Result<String, ToolError> {
        Ok("run-1".to_string())
    }

    fn start_temporary_run(
        &self,
        _: &SessionScope,
        _: &tool_gateway::ExecuteRunQuery,
        _: Option<&str>,
    ) -> Result<String, ToolError> {
        Ok("run-1".to_string())
    }

    fn run_output(&self, _: &SessionScope, id: &str) -> Result<RunOutput, ToolError> {
        Ok(RunOutput {
            id: id.to_string(),
            running: false,
            exit_code: Some(0),
            lines: vec!["hello".to_string()],
            full_output_path: None,
        })
    }

    fn propose_edit(
        &self,
        _: &SessionScope,
        query: &ProposeEditQuery,
    ) -> Result<ProposedEdit, ToolError> {
        let text = query.new_text.clone().unwrap_or_default();
        *self.proposed.lock().unwrap() = Some((query.path.clone(), text.clone()));
        Ok(ProposedEdit {
            path: query.path.clone(),
            outcome: tool_gateway::EditOutcome::Accepted,
            text: Some(text),
            hunks: 1,
            kept: 1,
        })
    }

    fn rename_symbol(
        &self,
        _: &SessionScope,
        query: &RenameSymbolQuery,
    ) -> Result<RenamedSymbol, ToolError> {
        Ok(RenamedSymbol {
            files: vec![query.path.clone()],
            edits: 3,
        })
    }

    fn format_file(
        &self,
        _: &SessionScope,
        query: &FileActionQuery,
    ) -> Result<FileActionOutcome, ToolError> {
        Ok(FileActionOutcome {
            path: query.path.clone(),
            changed: true,
            edits: 2,
        })
    }

    fn open_file(&self, _: &SessionScope, path: &Path) -> Result<(), ToolError> {
        self.opened.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }

    fn source_text(&self, _: &SessionScope, _: &str) -> Result<String, ToolError> {
        Ok("class Library {}\n".to_string())
    }

    fn repositories(&self, _: &SessionScope) -> Result<Vec<PathBuf>, ToolError> {
        Ok(vec![self.root.clone()])
    }

    fn git_status(&self, _: &SessionScope, query: &GitStatusQuery) -> Result<GitStatus, ToolError> {
        let mut files = vec![
            GitFileEntry {
                path: "src/Main.txt".to_string(),
                index: "M".to_string(),
                worktree: " ".to_string(),
            },
            GitFileEntry {
                path: "notes.md".to_string(),
                index: "?".to_string(),
                worktree: "?".to_string(),
            },
        ];
        if !query.include_untracked {
            files.retain(|file| file.index != "?");
        }
        Ok(GitStatus {
            repositories: vec![GitRepositoryStatus {
                path: String::new(),
                branch: Some("main".to_string()),
                files,
                total: 2,
                clipped: false,
            }],
        })
    }

    fn execute_terminal(
        &self,
        _: &SessionScope,
        _: &TerminalQuery,
    ) -> Result<TerminalOutput, ToolError> {
        Ok(TerminalOutput {
            output: "hello\n".to_string(),
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
        })
    }
}

struct Fixture {
    server: McpServer,
    token: String,
    headers: Vec<String>,
    fake: Arc<Fake>,
    _folder: TempDir,
}

fn fixture() -> Fixture {
    let folder = TempDir::new("mcp-idea-tools");
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("src/Main.txt"), "class Main\n    bad();\nend\n").unwrap();
    std::fs::write(folder.join("src/Other.txt"), "caller()\n").unwrap();
    let fake = Arc::new(Fake {
        root: folder.path().to_path_buf(),
        ..Fake::default()
    });
    let gateway = Arc::new(ToolGateway::with_approval(
        fake.clone(),
        AuditLog::default(),
        Arc::new(Allow),
    ));
    let server = McpServer::start(gateway).expect("start");
    let token = server
        .issue_token(SessionScope {
            session: "idea".to_string(),
            project: folder.path().to_path_buf(),
            cwd: folder.path().to_path_buf(),
        })
        .expect("token");
    let mut fixture = Fixture {
        server,
        token,
        headers: Vec::new(),
        fake,
        _folder: folder,
    };
    fixture.headers = connect(&fixture);
    fixture
}

fn post(fixture: &Fixture, headers: &[String], body: &Value) -> (u16, String) {
    let url = fixture.server.url();
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .expect("a local url");
    let mut stream = TcpStream::connect(address).expect("connect");
    let body = body.to_string();
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for header in headers {
        request.push_str(header);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(&body);
    stream.write_all(request.as_bytes()).expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    let head = response.split("\r\n\r\n").next().unwrap_or(&response);
    let status = head.split_whitespace().nth(1).unwrap_or("0");
    (status.parse().unwrap_or(0), response)
}

fn bearer(fixture: &Fixture) -> String {
    format!("Authorization: Bearer {}", fixture.token)
}

fn connect(fixture: &Fixture) -> Vec<String> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "idea-test", "version": "0"},
        },
    });
    let (status, reply) = post(fixture, &[bearer(fixture)], &body);
    assert_eq!(status, 200, "{reply}");
    let session = reply
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_string())
        })
        .expect("a session id");
    let headers = vec![
        bearer(fixture),
        format!("Mcp-Session-Id: {session}"),
        "MCP-Protocol-Version: 2025-06-18".to_string(),
    ];
    let done = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let _ = post(fixture, &headers, &done);
    headers
}

fn call(fixture: &Fixture, name: &str, arguments: Value) -> String {
    let body = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });
    let (status, reply) = post(fixture, &fixture.headers, &body);
    assert_eq!(status, 200, "{name}: {reply}");
    reply
}

#[test]
fn the_list_names_every_idea_tool_with_its_parameters() {
    let fixture = fixture();
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (status, reply) = post(&fixture, &fixture.headers, &body);
    assert_eq!(status, 200, "{reply}");
    for (name, parameter) in [
        ("analyze_calls", "symbolFqn"),
        ("build_project", "rebuild"),
        ("get_file_problems", "errorsOnly"),
        ("get_project_dependencies", "projectPath"),
        ("get_project_modules", "projectPath"),
        ("lint_files", "min_severity"),
        ("get_symbol_info", "filePath"),
        ("get_run_configurations", "filePath"),
        ("execute_run_configuration", "waitForExit"),
        ("create_new_file", "pathInProject"),
        ("get_all_open_file_paths", "projectPath"),
        ("list_directory_tree", "maxDepth"),
        ("open_file_in_editor", "filePath"),
        ("reformat_file", "path"),
        ("apply_patch", "input"),
        ("rename_refactoring", "symbolName"),
        ("read_file", "line_columns"),
        ("search_file", "includeExcluded"),
        ("search_regex", "paths"),
        ("search_symbol", "include_external"),
        ("search_text", "q"),
        ("skill_search", "mode"),
        ("execute_terminal_command", "truncateMode"),
        ("execute_tool", "command"),
        ("get_repositories", "projectPath"),
        ("git_status", "includeUntracked"),
    ] {
        assert!(reply.contains(name), "{name} is not listed: {reply}");
        assert!(
            reply.contains(parameter) || name == "read_file",
            "{name} does not name {parameter}: {reply}"
        );
    }
}

#[test]
fn a_read_of_chosen_lines_and_of_an_indentation_block_answers() {
    let fixture = fixture();

    let lines = call(
        &fixture,
        "read_file",
        json!({"file_path": "src/Main.txt", "mode": "lines", "start_line": 2, "end_line": 2}),
    );
    assert!(lines.contains("2|    bad();"), "{lines}");

    let block = call(
        &fixture,
        "read_file",
        json!({"file_path": "src/Main.txt", "mode": "indentation", "start_line": 2, "max_levels": 1}),
    );
    assert!(block.contains("lines 2-2 of 3"), "{block}");
    assert!(block.contains("2|    bad();"), "{block}");
}

#[test]
fn a_unified_patch_changes_the_text_through_the_edit_path() {
    let fixture = fixture();
    let patch = "\
--- a/src/Main.txt
+++ b/src/Main.txt
@@ -1,3 +1,3 @@
 class Main
-    bad();
+    good();
 end
";

    let answer = call(&fixture, "apply_patch", json!({"input": patch}));

    assert!(answer.contains("accepted"), "{answer}");
    let (path, text) = fixture
        .fake
        .proposed
        .lock()
        .unwrap()
        .clone()
        .expect("the edit reached the backend");
    assert!(path.ends_with("Main.txt"), "{}", path.display());
    assert!(text.contains("good();"), "{text}");
}

#[test]
fn a_patch_outside_the_project_is_refused() {
    let fixture = fixture();
    let patch = "\
--- a/../secret.txt
+++ b/../secret.txt
@@ -1 +1 @@
-one
+two
";

    let answer = call(&fixture, "apply_patch", json!({"input": patch}));

    assert!(answer.contains("isError"), "{answer}");
    assert!(answer.contains("outside the project"), "{answer}");
}

#[test]
fn the_file_search_takes_exclusions() {
    let fixture = fixture();

    let answer = call(
        &fixture,
        "search_file",
        json!({"q": "**/*.txt", "paths": ["src", "!src/generated"]}),
    );

    assert!(answer.contains("src/Main.txt"), "{answer}");
    assert!(answer.contains("src/Other.txt"), "{answer}");
    assert!(!answer.contains("Gen.txt"), "{answer}");
    assert!(!answer.contains("notes.md"), "{answer}");
}

#[test]
fn the_git_status_can_leave_untracked_files_out() {
    let fixture = fixture();

    let all = call(&fixture, "git_status", json!({"includeUntracked": true}));
    let tracked = call(&fixture, "git_status", json!({"includeUntracked": false}));

    assert!(
        all.contains("notes.md") && all.contains("src/Main.txt"),
        "{all}"
    );
    assert!(!tracked.contains("notes.md"), "{tracked}");
    assert!(tracked.contains("src/Main.txt"), "{tracked}");
}

#[test]
fn execute_tool_calls_a_tool_by_name() {
    let fixture = fixture();

    let json = call(
        &fixture,
        "execute_tool",
        json!({"command": "get_file_problems {\"filePath\": \"src/Main.txt\"}"}),
    );
    assert!(json.contains("cannot resolve"), "{json}");
    let key_value = call(
        &fixture,
        "execute_tool",
        json!({"command": "find_symbol query=Widget limit=5"}),
    );
    assert!(key_value.contains("class Widget"), "{key_value}");
}

#[test]
fn the_old_diagnostic_name_still_answers_as_before() {
    let fixture = fixture();

    let answer = call(&fixture, "get_diagnostics", json!({"path": "src/Main.txt"}));

    assert!(answer.contains("1 diagnostics in "), "{answer}");
    assert!(answer.contains(":2:3: error: cannot resolve"), "{answer}");
}

#[test]
fn every_new_tool_answers_on_the_project() {
    let fixture = fixture();
    std::fs::write(
        fixture.fake.file("src/Main.txt"),
        "class Main\n    bad();\n    good();\nend\n",
    )
    .unwrap();
    let calls = [
        (
            "analyze_calls",
            json!({"symbolFqn": "demo.Main.run", "analysisKind": "INCOMING_CALLS"}),
        ),
        (
            "get_file_problems",
            json!({"filePath": "src/Main.txt", "errorsOnly": true}),
        ),
        ("get_project_dependencies", json!({})),
        ("get_project_modules", json!({})),
        (
            "lint_files",
            json!({"files": ["src/Main.txt"], "min_severity": "warning"}),
        ),
        ("get_run_configurations", json!({})),
        (
            "get_run_configurations",
            json!({"filePath": "src/Main.txt"}),
        ),
        (
            "execute_run_configuration",
            json!({"configurationName": "Main", "waitForExit": true}),
        ),
        (
            "execute_run_configuration",
            json!({"filePath": "src/Main.txt", "line": 1, "waitForExit": false}),
        ),
        (
            "create_new_file",
            json!({"pathInProject": "src/New.txt", "text": "new\n"}),
        ),
        ("get_all_open_file_paths", json!({})),
        (
            "list_directory_tree",
            json!({"directoryPath": "src", "maxDepth": 1}),
        ),
        ("open_file_in_editor", json!({"filePath": "src/Main.txt"})),
        ("reformat_file", json!({"path": "src/Main.txt"})),
        (
            "rename_refactoring",
            json!({"pathInProject": "src/Main.txt", "symbolName": "Main", "newName": "App"}),
        ),
        ("search_regex", json!({"q": "bad"})),
        ("search_symbol", json!({"q": "Widget"})),
        (
            "skill_search",
            json!({"mode": "file", "q": "**/*.txt", "paths": ["src"]}),
        ),
        (
            "execute_terminal_command",
            json!({"command": "echo hello", "timeout": 5000}),
        ),
        ("get_repositories", json!({})),
        ("git_status", json!({})),
    ];
    for (name, arguments) in calls {
        let reply = call(&fixture, name, arguments);
        assert!(
            reply.contains("\"result\"") && !reply.contains("\"isError\":true"),
            "{name}: {reply}"
        );
        assert!(
            reply.contains("content") || reply.contains("structuredContent"),
            "{name}: {reply}"
        );
    }
}
