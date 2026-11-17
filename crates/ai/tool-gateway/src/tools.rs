use std::sync::LazyLock;

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    /// Reads IDE state and runs nothing of the project.
    Read,
    /// Runs project code, so it needs the permission of the user.
    Execute,
}

impl ToolClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Execute => "execute",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub class: ToolClass,
    pub input_schema: Value,
}

pub(crate) const EDITOR_CONTEXT: &str = "get_editor_context";
pub(crate) const FIND_SYMBOL: &str = "find_symbol";
pub(crate) const SYMBOL_INFO: &str = "get_symbol_info";
pub(crate) const DIAGNOSTICS: &str = "get_diagnostics";
pub(crate) const DEBUG_STATE: &str = "debug_get_state";

/// Every tool, for harnesses that list what may run without a question.
pub const TOOL_NAMES: &[&str] = &[
    EDITOR_CONTEXT,
    FIND_SYMBOL,
    SYMBOL_INFO,
    DIAGNOSTICS,
    DEBUG_STATE,
];

/// Tools arrive together with their services, not as one package.
pub(crate) static TOOLS: LazyLock<Vec<ToolSpec>> = LazyLock::new(|| {
    vec![
        ToolSpec {
            name: EDITOR_CONTEXT,
            description: "The file open in the IDE, the caret position, the selection \
                          and the other open files.",
            class: ToolClass::Read,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: FIND_SYMBOL,
            description: "Finds declarations by name in the project. Every hit comes with \
                          `path:line:content` and two lines of context. Plain text search \
                          stays the better tool for strings and comments.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "A symbol name or a part of it"},
                    "kind": {"type": "string", "description": "Only symbols of this kind"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "after": {
                        "type": "string",
                        "description": "The continuation token of a truncated answer",
                    },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SYMBOL_INFO,
            description: "What the symbol at a position is: kind, signature, documentation \
                          and where it is declared, with the line of the declaration. \
                          Answers from the language server, not from text.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "column": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based, in characters",
                    },
                },
                "required": ["path", "line", "column"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DIAGNOSTICS,
            description: "Compiler errors and warnings the IDE knows, as \
                          `path:line:column: severity: message` with the line itself. \
                          One file or the whole project.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Only this file, relative to the project root or absolute",
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["error", "warning", "info", "hint"],
                        "description": "The least severe kind to include, warning by default",
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                    "after": {
                        "type": "string",
                        "description": "The continuation token of a truncated answer",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_STATE,
            description: "The state of the debug session: running or stopped, the reason, \
                          the selected thread, the top frames of its stack, the variables \
                          of its top frame and the breakpoints. Says so plainly when no \
                          session is running. Read-only, it never resumes or stops the \
                          debuggee.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "frames": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "How many top frames to show, 10 by default",
                    },
                    "variables": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 200,
                        "description": "How many variables of the top frame to show, 20 by default",
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 5,
                        "description": "How many levels of children of a variable to follow",
                    },
                    "length": {
                        "type": "integer",
                        "minimum": 20,
                        "maximum": 2000,
                        "description": "The longest value to show, in characters",
                    },
                },
                "additionalProperties": false,
            }),
        },
    ]
});
