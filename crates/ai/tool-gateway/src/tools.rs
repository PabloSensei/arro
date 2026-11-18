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
pub(crate) const FIND_USAGES: &str = "find_usages";
pub(crate) const TYPE_HIERARCHY: &str = "get_type_hierarchy";
pub(crate) const CALL_HIERARCHY: &str = "get_call_hierarchy";
pub(crate) const PROJECT_STRUCTURE: &str = "get_project_structure";
pub(crate) const SEARCH_TEXT: &str = "search_text";
pub(crate) const READ_FILE: &str = "read_file";
pub(crate) const VCS_CHANGES: &str = "get_vcs_changes";
pub(crate) const PROPOSE_EDIT: &str = "propose_edit";
pub(crate) const RENAME_SYMBOL: &str = "rename_symbol";
pub(crate) const ORGANIZE_IMPORTS: &str = "organize_imports";
pub(crate) const FORMAT_FILE: &str = "format_file";
pub(crate) const BUILD_PROJECT: &str = "build_project";
pub(crate) const RUN_TESTS: &str = "run_tests";
pub(crate) const LIST_RUN_CONFIGURATIONS: &str = "list_run_configurations";
pub(crate) const RUN_CONFIGURATION: &str = "run_configuration";
pub(crate) const GET_RUN_OUTPUT: &str = "get_run_output";
pub(crate) const STOP_RUN: &str = "stop_run";

/// Every tool, for harnesses that list what may run without a question.
pub const TOOL_NAMES: &[&str] = &[
    EDITOR_CONTEXT,
    FIND_SYMBOL,
    SYMBOL_INFO,
    DIAGNOSTICS,
    DEBUG_STATE,
    FIND_USAGES,
    TYPE_HIERARCHY,
    CALL_HIERARCHY,
    PROJECT_STRUCTURE,
    SEARCH_TEXT,
    READ_FILE,
    VCS_CHANGES,
    PROPOSE_EDIT,
    RENAME_SYMBOL,
    ORGANIZE_IMPORTS,
    FORMAT_FILE,
    BUILD_PROJECT,
    RUN_TESTS,
    LIST_RUN_CONFIGURATIONS,
    RUN_CONFIGURATION,
    GET_RUN_OUTPUT,
    STOP_RUN,
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
        ToolSpec {
            name: FIND_USAGES,
            description: "Every place a symbol is used, with its file, line, column and the \
                          line itself. Give a place in a file, or only a name. Cheaper and \
                          more exact than searching the text for the name.",
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
                    "name": {
                        "type": "string",
                        "description": "A declaration name, when no place is given",
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: TYPE_HIERARCHY,
            description: "The types above and below the type at a place: its parents and its \
                          children, each with its declaration. Answers from the language \
                          server, not from the text.",
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
            name: CALL_HIERARCHY,
            description: "The methods that call the method at a place, down to the given \
                          number of levels. A level of one shows its direct callers.",
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
                    "depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 5,
                        "description": "How many levels of callers to follow, 1 by default",
                    },
                },
                "required": ["path", "line", "column"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: PROJECT_STRUCTURE,
            description: "The modules of the project with their source and test roots, the \
                          build system, the runtime and the language level, and the \
                          dependencies of every module. Saves reading the build files.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "How many modules to show, 50 by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SEARCH_TEXT,
            description: "Searches the project the way Find in Files does: plain text or a \
                          regular expression, an optional file mask, with the build output \
                          and the excluded folders left out. Use it for strings and \
                          comments; `find_symbol` is better for declarations.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "regex": {
                        "type": "boolean",
                        "description": "Read the pattern as a regular expression",
                    },
                    "mask": {
                        "type": "string",
                        "description": "File patterns separated by spaces, as Find in Files takes them",
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                },
                "required": ["pattern"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: READ_FILE,
            description: "The text of a file as the editor holds it, including edits that \
                          are not saved, with a range of lines. Cheaper than reading the \
                          file with a shell when it is open or long.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "start_line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based and inclusive",
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: VCS_CHANGES,
            description: "The files the working tree changed against the last commit with \
                          their status, and the diff of one file when a path is given. Uses \
                          the repository the IDE already reads.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A changed file of the project to diff against HEAD",
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: PROPOSE_EDIT,
            description: "Offers an edit of a file to the user and waits for the decision. \
                          Give the whole new text, or replacements of the text the file has. \
                          The editor shows the hunks, and the user accepts or rejects each of \
                          them. Nothing is written until every hunk is decided.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "new_text": {
                        "type": "string",
                        "description": "The whole text the file should have after the edit",
                    },
                    "replacements": {
                        "type": "array",
                        "description": "The pieces to replace, one after another",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {
                                    "type": "string",
                                    "description": "Exact text that stands once in the file",
                                },
                                "new_text": {"type": "string"},
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: RENAME_SYMBOL,
            description: "Renames the symbol at a place through the language service, so \
                          every use follows. The window shows the files it changes and \
                          applies them after the user confirms.",
            class: ToolClass::Execute,
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
                    "new_name": {"type": "string", "description": "The name the symbol gets"},
                },
                "required": ["path", "line", "column", "new_name"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: ORGANIZE_IMPORTS,
            description: "Sorts and removes the imports of a file with the action of the \
                          language service that Organize Imports runs. The open editor \
                          shows the change at once.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: FORMAT_FILE,
            description: "Formats a file as the language service formats it. The open editor \
                          shows the change at once.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: BUILD_PROJECT,
            description: "Builds the project with the build tool of the IDE, not by running \
                          one in a terminal, and answers the verdict, the compile errors as \
                          `path:line: message` and the time. The build shows in the Run \
                          window as any other build. Ask first: this tool runs code of the \
                          build and may take minutes.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "module": {
                        "type": "string",
                        "description": "Only this module of the project; the whole project by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: RUN_TESTS,
            description: "Runs tests the way the Tests window does and answers how many \
                          passed, failed and were skipped, with the name, message and the \
                          short stack of every failure. The whole log is read by \
                          `get_run_output` with the id of the answer. A case needs its \
                          suite, a suite is a class or a file of tests.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "module": {"type": "string", "description": "Only this module"},
                    "package": {
                        "type": "string",
                        "description": "Every test of this package or folder",
                    },
                    "suite": {
                        "type": "string",
                        "description": "A class or a file of tests, in the terms the project uses",
                    },
                    "case": {
                        "type": "string",
                        "description": "One test of the suite; it needs `suite`",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: LIST_RUN_CONFIGURATIONS,
            description: "The run configurations of the project the user can start, with \
                          their names and kinds. Read-only.",
            class: ToolClass::Read,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: RUN_CONFIGURATION,
            description: "Starts a run configuration of the project in the Run window and \
                          answers the id of the run. `get_run_output` reads its output and \
                          `stop_run` stops it. The user sees it as any other run and can \
                          stop it too.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name from `list_run_configurations`",
                    },
                },
                "required": ["name"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_RUN_OUTPUT,
            description: "The state of a run, its exit code when it ended and the last \
                          lines it printed, by the id `run_tests`, `build_project` or \
                          `run_configuration` answered. Read-only.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The id of the run"},
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: STOP_RUN,
            description: "Stops the program of a run by its id, as the Stop button of the \
                          Run window does.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The id of the run"},
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
        },
    ]
});

#[cfg(test)]
mod tests {
    use super::{TOOL_NAMES, TOOLS, ToolClass};

    #[test]
    fn every_tool_is_listed_once_with_an_object_schema() {
        assert_eq!(TOOL_NAMES.len(), TOOLS.len());
        for (index, spec) in TOOLS.iter().enumerate() {
            assert_eq!(spec.name, TOOL_NAMES[index]);
            assert!(!spec.description.is_empty(), "{}", spec.name);
            assert_eq!(spec.input_schema["type"], "object", "{}", spec.name);
        }
        let mut names: Vec<&str> = TOOL_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TOOL_NAMES.len(), "a name is listed twice");
    }

    #[test]
    fn the_read_tools_of_the_project_are_listed() {
        for name in [
            "find_usages",
            "get_type_hierarchy",
            "get_call_hierarchy",
            "get_project_structure",
            "search_text",
            "read_file",
            "get_vcs_changes",
        ] {
            assert!(TOOL_NAMES.contains(&name), "{name} is missing");
        }
    }

    #[test]
    fn the_edit_tools_need_the_user() {
        for name in [
            "propose_edit",
            "rename_symbol",
            "organize_imports",
            "format_file",
        ] {
            let spec = TOOLS.iter().find(|spec| spec.name == name).expect(name);
            assert_eq!(spec.class, ToolClass::Execute, "{name}");
            assert_eq!(spec.input_schema["type"], "object", "{name}");
        }
    }
}
