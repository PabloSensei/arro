//! The database tools of IDEA's server in the window: a statement of an agent runs through
//! the console of the Database window and its result is read by pages. The connection is
//! scripted, as in the tests of the window, so no driver and no server are reached.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use database::{
    Column, ColumnKind, DriverKind, ResultSet, Source, SourceId, Table, TableKind, Value,
};
use serde_json::Value as Json;
use serde_json::json;
use tool_gateway::{
    Approval, ApprovalAnswer, ApprovalRequest, AuditLog, SessionScope, ToolGateway,
};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::DATABASE_TOOL;

use crate::agents::AgentLanguages;
use crate::database_panel::ActiveDatabase;
use crate::window_tests::{open_window, sample_project, wait_for_signal};

const SOURCE: &str = "owners";

struct Allow;

impl Approval for Allow {
    fn ask(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Once
    }
}

fn source() -> Source {
    Source {
        id: SourceId::new(SOURCE),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        url: "mem:petclinic".into(),
        user: "sa".into(),
        module: None,
        driver_jar: None,
    }
}

fn tables() -> Vec<Table> {
    vec![Table {
        schema: "PUBLIC".into(),
        name: "OWNERS".into(),
        kind: TableKind::Table,
        columns: vec![Column {
            name: "ID".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: false,
            primary_key: true,
        }],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        partial: false,
    }]
}

fn rows() -> ResultSet {
    ResultSet {
        columns: vec![Column {
            name: "ID".into(),
            type_name: "INTEGER".into(),
            kind: ColumnKind::Integer,
            nullable: false,
            primary_key: true,
        }],
        rows: vec![vec![Value::Integer(1)], vec![Value::Integer(2)]],
        update_count: None,
        truncated: false,
    }
}

fn gateway(cx: &TestAppContext) -> Arc<ToolGateway> {
    let tools = cx.read(|cx| cx.global::<AgentLanguages>().0.clone());
    Arc::new(ToolGateway::with_approval(
        tools,
        AuditLog::default(),
        Arc::new(Allow),
    ))
}

fn call(
    cx: &mut TestAppContext,
    gateway: &Arc<ToolGateway>,
    root: &std::path::Path,
    tool: &str,
    input: Json,
) -> Result<String, String> {
    let gateway = gateway.clone();
    let scope = SessionScope {
        session: "mcp-database".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    };
    let tool_name = tool.to_string();
    let in_thread = tool_name.clone();
    let asked = thread::spawn(move || {
        gateway
            .call(&scope, &in_thread, &input)
            .map_err(|error| error.to_string())
    });
    wait_for_signal(cx, "the answer of the tool", |_| asked.is_finished());
    asked.join().expect("the thread of the call")
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_statement_of_an_agent_runs_in_the_console_and_is_read_by_pages(cx: &mut TestAppContext) {
    let root = sample_project("mcp-database");
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    let panel = cx.read(|cx| cx.global::<ActiveDatabase>().0.clone());
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(
            source(),
            tables(),
            rows(),
            None,
            Arc::new(AtomicBool::new(false)),
            cx,
        );
    });
    wait_for_signal(cx, "the source of the window", |cx| {
        panel.read_with(cx, |panel, _| !panel.sources().is_empty())
    });
    let gateway = gateway(cx);

    let executed = call(
        cx,
        &gateway,
        &root,
        "execute_sql_query",
        json!({"connectionId": SOURCE, "queryText": "SELECT ID FROM OWNERS"}),
    )
    .expect("the statement runs");
    let value: Json = serde_json::from_str(&executed).expect("the answer is JSON");
    assert_eq!(value["rowCount"], 2, "{value}");
    assert_eq!(value["rows"], "ID\n1\n2\n", "{value}");
    let id = value["resultSetId"].as_str().expect("a result set id");

    let fetched = call(
        cx,
        &gateway,
        &root,
        "fetch_query_result",
        json!({"resultSetId": id, "offset": 1}),
    )
    .expect("the next page of the result");
    let page: Json = serde_json::from_str(&fetched).expect("the answer is JSON");
    assert_eq!(page["rowCount"], 1, "{page}");
    assert_eq!(page["rows"], "ID\n2\n", "{page}");
    assert_eq!(page["hasMore"], false, "{page}");

    let listed = call(cx, &gateway, &root, "list_database_connections", json!({}))
        .expect("the connections of the window");
    assert!(listed.contains("\"id\":\"owners\""), "{listed}");
    assert!(listed.contains("PetClinic"), "{listed}");
    let objects = call(
        cx,
        &gateway,
        &root,
        "list_schema_objects",
        json!({"connectionId": SOURCE, "schemaName": "PUBLIC"}),
    )
    .expect("the objects of the schema");
    assert!(objects.contains("OWNERS"), "{objects}");
}
