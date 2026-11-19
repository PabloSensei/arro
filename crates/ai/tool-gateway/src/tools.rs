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
pub(crate) const SET_BREAKPOINT: &str = "set_breakpoint";
pub(crate) const REMOVE_BREAKPOINT: &str = "remove_breakpoint";
pub(crate) const LIST_BREAKPOINTS: &str = "list_breakpoints";
pub(crate) const DEBUG_RUN: &str = "debug_run";
pub(crate) const DEBUG_WAIT: &str = "debug_wait";
pub(crate) const DEBUG_STACK: &str = "debug_stack";
pub(crate) const DEBUG_VARIABLES: &str = "debug_variables";
pub(crate) const DEBUG_EVALUATE: &str = "debug_evaluate";
pub(crate) const DEBUG_STEP: &str = "debug_step";
pub(crate) const DEBUG_STOP: &str = "debug_stop";
pub(crate) const ANALYZE_CALLS: &str = "analyze_calls";
pub(crate) const GET_FILE_PROBLEMS: &str = "get_file_problems";
pub(crate) const GET_PROJECT_DEPENDENCIES: &str = "get_project_dependencies";
pub(crate) const GET_PROJECT_MODULES: &str = "get_project_modules";
pub(crate) const LINT_FILES: &str = "lint_files";
pub(crate) const GET_RUN_CONFIGURATIONS: &str = "get_run_configurations";
pub(crate) const EXECUTE_RUN_CONFIGURATION: &str = "execute_run_configuration";
pub(crate) const CREATE_NEW_FILE: &str = "create_new_file";
pub(crate) const GET_ALL_OPEN_FILE_PATHS: &str = "get_all_open_file_paths";
pub(crate) const LIST_DIRECTORY_TREE: &str = "list_directory_tree";
pub(crate) const OPEN_FILE_IN_EDITOR: &str = "open_file_in_editor";
pub(crate) const REFORMAT_FILE: &str = "reformat_file";
pub(crate) const APPLY_PATCH: &str = "apply_patch";
pub(crate) const RENAME_REFACTORING: &str = "rename_refactoring";
pub(crate) const SEARCH_FILE: &str = "search_file";
pub(crate) const SEARCH_REGEX: &str = "search_regex";
pub(crate) const SEARCH_SYMBOL: &str = "search_symbol";
pub(crate) const SKILL_SEARCH: &str = "skill_search";
pub(crate) const EXECUTE_TERMINAL_COMMAND: &str = "execute_terminal_command";
pub(crate) const EXECUTE_TOOL: &str = "execute_tool";
pub(crate) const GET_REPOSITORIES: &str = "get_repositories";
pub(crate) const GIT_STATUS: &str = "git_status";
pub(crate) const XDEBUG_STATUS: &str = "xdebug_get_debugger_status";
pub(crate) const XDEBUG_START: &str = "xdebug_start_debugger_session";
pub(crate) const XDEBUG_CONTROL: &str = "xdebug_control_session";
pub(crate) const XDEBUG_RUN_TO_LINE: &str = "xdebug_run_to_line";
pub(crate) const XDEBUG_THREADS: &str = "xdebug_get_threads";
pub(crate) const XDEBUG_STACK: &str = "xdebug_get_stack";
pub(crate) const XDEBUG_FRAME_VALUES: &str = "xdebug_get_frame_values";
pub(crate) const XDEBUG_VALUE_BY_PATH: &str = "xdebug_get_value_by_path";
pub(crate) const XDEBUG_EVALUATE: &str = "xdebug_evaluate_expression";
pub(crate) const XDEBUG_SET_VARIABLE: &str = "xdebug_set_variable";
pub(crate) const XDEBUG_LIST_BREAKPOINTS: &str = "xdebug_list_breakpoints";
pub(crate) const XDEBUG_SET_BREAKPOINT: &str = "xdebug_set_breakpoint";
pub(crate) const XDEBUG_REMOVE_BREAKPOINT: &str = "xdebug_remove_breakpoint";
pub(crate) const DATABASE_CONNECTIONS: &str = "list_database_connections";
pub(crate) const DATABASE_TEST: &str = "test_database_connection";
pub(crate) const DATABASE_CREATE: &str = "create_database_connection";
pub(crate) const DATABASE_EDIT: &str = "edit_database_connection";
pub(crate) const DATABASE_SCHEMAS: &str = "list_database_schemas";
pub(crate) const DATABASE_INTROSPECT: &str = "introspect_schema";
pub(crate) const DATABASE_KINDS: &str = "list_schema_object_kinds";
pub(crate) const DATABASE_OBJECTS: &str = "list_schema_objects";
pub(crate) const DATABASE_DESCRIBE: &str = "get_database_object_description";
pub(crate) const DATABASE_EXECUTE: &str = "execute_sql_query";
pub(crate) const DATABASE_FETCH: &str = "fetch_query_result";
pub(crate) const DATABASE_PREVIEW: &str = "preview_table_data";
pub(crate) const DATABASE_RECENT: &str = "list_recent_sql_queries";
pub(crate) const DATABASE_CANCEL: &str = "cancel_sql_query";

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
    SET_BREAKPOINT,
    REMOVE_BREAKPOINT,
    LIST_BREAKPOINTS,
    DEBUG_RUN,
    DEBUG_WAIT,
    DEBUG_STACK,
    DEBUG_VARIABLES,
    DEBUG_EVALUATE,
    DEBUG_STEP,
    DEBUG_STOP,
    ANALYZE_CALLS,
    GET_FILE_PROBLEMS,
    GET_PROJECT_DEPENDENCIES,
    GET_PROJECT_MODULES,
    LINT_FILES,
    GET_RUN_CONFIGURATIONS,
    EXECUTE_RUN_CONFIGURATION,
    CREATE_NEW_FILE,
    GET_ALL_OPEN_FILE_PATHS,
    LIST_DIRECTORY_TREE,
    OPEN_FILE_IN_EDITOR,
    REFORMAT_FILE,
    APPLY_PATCH,
    RENAME_REFACTORING,
    SEARCH_FILE,
    SEARCH_REGEX,
    SEARCH_SYMBOL,
    SKILL_SEARCH,
    EXECUTE_TERMINAL_COMMAND,
    EXECUTE_TOOL,
    GET_REPOSITORIES,
    GIT_STATUS,
    XDEBUG_STATUS,
    XDEBUG_START,
    XDEBUG_CONTROL,
    XDEBUG_RUN_TO_LINE,
    XDEBUG_THREADS,
    XDEBUG_STACK,
    XDEBUG_FRAME_VALUES,
    XDEBUG_VALUE_BY_PATH,
    XDEBUG_EVALUATE,
    XDEBUG_SET_VARIABLE,
    XDEBUG_LIST_BREAKPOINTS,
    XDEBUG_SET_BREAKPOINT,
    XDEBUG_REMOVE_BREAKPOINT,
    DATABASE_CONNECTIONS,
    DATABASE_TEST,
    DATABASE_CREATE,
    DATABASE_EDIT,
    DATABASE_SCHEMAS,
    DATABASE_INTROSPECT,
    DATABASE_KINDS,
    DATABASE_OBJECTS,
    DATABASE_DESCRIBE,
    DATABASE_EXECUTE,
    DATABASE_FETCH,
    DATABASE_PREVIEW,
    DATABASE_RECENT,
    DATABASE_CANCEL,
];

/// The groups of the Exposed Tools table, in the order the reference lists them.
pub const TOOL_GROUPS: [&str; 11] = [
    "Analysis",
    "Code Insight",
    "Execution",
    "Files",
    "Editing",
    "Read",
    "Search",
    "Terminal",
    "Universal",
    "VCS",
    "Debugger",
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
                    "filePath": {
                        "type": "string",
                        "description": "The same as path, the name IDEA uses",
                    },
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "column": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based, in characters",
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "The project of the window, when the agent knows it",
                    },
                },
                "required": ["line", "column"],
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
                    "q": {
                        "type": "string",
                        "description": "The same as pattern, the name IDEA uses",
                    },
                    "regex": {
                        "type": "boolean",
                        "description": "Read the pattern as a regular expression",
                    },
                    "mask": {
                        "type": "string",
                        "description": "File patterns separated by spaces, as Find in Files takes them",
                    },
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Folders to search, a leading ! excludes",
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                    "timeout": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Milliseconds; the answer keeps what was found by then",
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "The project of the window, when the agent knows it",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: READ_FILE,
            description: "The text of a file with numbered lines. With `mode` it reads a slice, \
                          chosen lines, columns, offsets or an indentation block; without it, the \
                          old `path` and `start_line`/`end_line` range. The editor text wins over \
                          the disk, so unsaved edits are read. A `jar://`, `jrt://` or `jdt://` \
                          address, and a `.class` the language service can decompile, answer \
                          through the decompiler of the language.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "file_path": {
                        "type": "string",
                        "description": "The same as path, the name IDEA uses",
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["slice", "lines", "line_columns", "offsets", "indentation"],
                        "description": "How to read; slice when absent",
                    },
                    "start_line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based and inclusive",
                    },
                    "max_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2000,
                        "description": "How many lines the answer carries at most",
                    },
                    "start_column": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "end_column": {"type": "integer", "minimum": 1, "description": "1-based and inclusive"},
                    "start_offset": {"type": "integer", "minimum": 0, "description": "0-based, in characters"},
                    "end_offset": {"type": "integer", "minimum": 0, "description": "0-based, in characters"},
                    "context_lines": {"type": "integer", "minimum": 0, "description": "Lines around the range"},
                    "max_levels": {"type": "integer", "minimum": 1, "description": "For indentation"},
                    "include_siblings": {"type": "boolean", "description": "For indentation"},
                    "include_header": {"type": "boolean", "description": "For indentation"},
                    "projectPath": {
                        "type": "string",
                        "description": "The project of the window, when the agent knows it",
                    },
                },
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
                    "rebuild": {
                        "type": "boolean",
                        "description": "Build every module again; does not act with filesToRebuild",
                    },
                    "filesToRebuild": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Only these files of the project",
                    },
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "projectPath": {
                        "type": "string",
                        "description": "The project of the window, when the agent knows it",
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
        ToolSpec {
            name: SET_BREAKPOINT,
            description: "Puts a breakpoint on a line of a file. The user sees it in the \
                          editor gutter and in the Breakpoints window, as one set by hand. \
                          A condition stops only when it is true; a log message prints \
                          instead of stopping.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "condition": {
                        "type": "string",
                        "description": "An expression the program evaluates at the line",
                    },
                    "log_message": {
                        "type": "string",
                        "description": "What the console prints instead of stopping the program",
                    },
                },
                "required": ["path", "line"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: REMOVE_BREAKPOINT,
            description: "Takes a breakpoint away from a line, as the same key in the gutter \
                          does. Read-only for the program.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A file of the project, relative to its root or absolute",
                    },
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                },
                "required": ["path", "line"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: LIST_BREAKPOINTS,
            description: "Every breakpoint of the project with its line, whether it is on, \
                          its condition and its log message. Read-only.",
            class: ToolClass::Read,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: DEBUG_RUN,
            description: "Starts a test suite or case, or a run configuration, under the \
                          debugger the way Shift+F9 does, and waits for the first stop. \
                          Answers the id of the session and where the program stopped. \
                          `debug_wait` waits again, `debug_stack` and `debug_variables` read \
                          the stop. The user watches the same session in the Debug window \
                          and may continue or stop it.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "module": {"type": "string", "description": "Only this module"},
                    "suite": {
                        "type": "string",
                        "description": "A class or a file of tests, in the terms the project uses",
                    },
                    "case": {
                        "type": "string",
                        "description": "One test of the suite; it needs `suite`",
                    },
                    "configuration": {
                        "type": "string",
                        "description": "The name from `list_run_configurations`",
                    },
                    "wait_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 120,
                        "description": "How long to wait for the first stop, 30 by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_WAIT,
            description: "Waits for the debugged program to stop or the session to end, up \
                          to the given number of seconds, and answers the reason, the \
                          thread and the top frame. Read-only, it never resumes or stops \
                          the program.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 120,
                        "description": "How long to wait, 10 by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_STACK,
            description: "The frames of the stopped thread with their places, the top one \
                          first. `debug_variables` reads a frame by its index. Read-only.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread": {
                        "type": "integer",
                        "description": "The thread by id; the selected thread by default",
                    },
                    "frames": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "How many top frames to show, 10 by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_VARIABLES,
            description: "The variables of a stopped frame, grouped by scope, with children \
                          down to the given depth and values cut to the given length. \
                          Read-only.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "frame": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The index of a frame of `debug_stack`, zero by default",
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
                    "variables": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "How many variables of a scope to show, 50 by default",
                    },
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_EVALUATE,
            description: "Evaluates an expression in a stopped frame, as the console of the \
                          debugger does, and answers the value and its type. The expression \
                          may change the program, so the user is asked first.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string"},
                    "frame": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The index of a frame of `debug_stack`, zero by default",
                    },
                },
                "required": ["expression"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_STEP,
            description: "Moves the stopped program: `over` a line, `into` a call, `out` of \
                          a method or `continue` to the next breakpoint, and answers where \
                          it stopped. The user may step it too.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["over", "into", "out", "continue"],
                    },
                    "seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 120,
                        "description": "How long to wait for the next stop, 10 by default",
                    },
                },
                "required": ["action"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DEBUG_STOP,
            description: "Ends the debug session and the program under it, as the Stop \
                          button of the Debug window does.",
            class: ToolClass::Execute,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: ANALYZE_CALLS,
            description: "The call tree of a symbol by its fully qualified name: the calls that \
                          reach it or the calls it makes. Every node names its `filePath` and its \
                          `treePath`, the place among its siblings that `treePath` and \
                          `childOffset` take.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbolFqn": {"type": "string", "description": "A fully qualified name or an exact signature, not a path"},
                    "analysisKind": {"type": "string", "enum": ["INCOMING_CALLS", "OUTGOING_CALLS"]},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 20, "description": "5 by default, 0 for the root alone"},
                    "maxChildren": {"type": "integer", "minimum": 1, "maximum": 200, "description": "50 by default"},
                    "maxNodes": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "1000 by default"},
                    "treePath": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "The node to start at, the root is []"},
                    "childOffset": {"type": "integer", "minimum": 0, "description": "How many children to skip, 0 by default"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["symbolFqn", "analysisKind"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_FILE_PROBLEMS,
            description: "The problems of the inspections of one file, each with its severity, \
                          its description and its place from 1. The answer is JSON: `filePath` and \
                          `problems` with `severity`, `description`, `line` and `column`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": {"type": "string", "description": "Relative to the project root or absolute"},
                    "errorsOnly": {"type": "boolean", "description": "Warnings and worse by default"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["filePath"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_PROJECT_DEPENDENCIES,
            description: "The dependencies of the project by the name of the library. The answer \
                          is JSON: `dependencies` with `name` and the `modules` that use it.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_PROJECT_MODULES,
            description: "The modules of the project with their name, type, folder and roots. The \
                          answer is JSON: `modules` with `name`, `type`, `path`, `sourceRoots` and \
                          `testRoots`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: LINT_FILES,
            description: "The problems of the given files. A file that was not analyzed says why, \
                          and `more` says that a file has more problems than the answer carries. \
                          The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "files": {"type": "array", "items": {"type": "string"}, "description": "Files of the project"},
                    "min_severity": {"type": "string", "enum": ["warning", "error"], "description": "warning by default"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["files"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_RUN_CONFIGURATIONS,
            description: "The run configurations of the project, or the gutter marks of one file. \
                          The answer is JSON: a configuration names its kind, command line, working \
                          directory, environment and whether a run may override them; a mark names \
                          `kind`, `name`, `filePath`, `line` and `framework`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": {"type": "string", "description": "Only the run points of this file"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: EXECUTE_RUN_CONFIGURATION,
            description: "Starts a run configuration, or a temporary one at a place of a file, and \
                          answers the snapshot of its output, `exitCode`, `fullOutputPath`, \
                          `timedOut` and whether it still runs. Asks the user first unless brave \
                          mode allows trusted projects. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "configurationName": {"type": "string", "description": "From get_run_configurations"},
                    "filePath": {"type": "string", "description": "A temporary configuration at this file"},
                    "line": {"type": "integer", "minimum": 1, "description": "The place of the run mark, 1-based"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds to wait"},
                    "waitForExit": {"type": "boolean", "description": "Wait for the end; true by default"},
                    "programArguments": {"type": "string", "description": "Only when the configuration allows it"},
                    "workingDirectory": {"type": "string", "description": "Only when the configuration allows it"},
                    "envs": {"type": "array", "items": {"type": "string"}, "description": "NAME=VALUE, only when the configuration allows it"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: CREATE_NEW_FILE,
            description: "Creates a file of the project with its parent folders, through the same \
                          edit path as `propose_edit`, so the user sees it and Undo takes it back. \
                          The answer is JSON: `filePath` and `created`.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pathInProject": {"type": "string", "description": "Relative to the project root"},
                    "text": {"type": "string", "description": "The text of the new file, empty by default"},
                    "overwrite": {"type": "boolean", "description": "False refuses a path that exists"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["pathInProject"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_ALL_OPEN_FILE_PATHS,
            description: "The files open in the editors of the window, relative to the project. The \
                          answer is JSON: `files`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: LIST_DIRECTORY_TREE,
            description: "The tree of a folder of the project as text, one entry per line. Each line \
                          names whether the entry is a directory and its depth.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "directoryPath": {"type": "string", "description": "Relative to the project root, the root by default"},
                    "maxDepth": {"type": "integer", "minimum": 1, "maximum": 20, "description": "3 by default"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: OPEN_FILE_IN_EDITOR,
            description: "Opens a file of the project in an editor of the window.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": {"type": "string", "description": "Relative to the project root or absolute"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["filePath"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: REFORMAT_FILE,
            description: "Formats a file through the formatting of the language service, on the same \
                          edit path as `propose_edit`, so the window shows it and Undo takes it back.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Relative to the project root"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: APPLY_PATCH,
            description: "Applies a patch in the `*** Begin Patch` form or a unified git diff. Adds, \
                          updates, deletes and moves files, and every path has to stay inside the \
                          project. Each file goes through the edit path of `propose_edit`. The \
                          answer is JSON with one `outcome` per `filePath`.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string", "description": "The patch text"},
                    "patch": {"type": "string", "description": "The same as input"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: RENAME_REFACTORING,
            description: "Renames every reference of a symbol of a file. The window shows the files it \
                          changes and applies them after the user confirms. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pathInProject": {"type": "string", "description": "Relative to the project root"},
                    "symbolName": {"type": "string", "description": "The declaration to rename"},
                    "newName": {"type": "string", "description": "The name it gets"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["pathInProject", "symbolName", "newName"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SEARCH_FILE,
            description: "The files of the project whose path matches a glob such as `**/*.txt`. \
                          `paths` keeps the search in the named folders and a leading `!` excludes \
                          one. The answer is JSON: `files`, `total`, `more` and `timedOut`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "q": {"type": "string", "description": "A glob of the path, for example **/*.txt"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders to search, a leading ! excludes"},
                    "includeExcluded": {"type": "boolean", "description": "Look in folders the project excludes too"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds; the answer keeps what was found by then"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["q"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SEARCH_REGEX,
            description: "The matches of a regular expression with the text of their line and from 1 \
                          the line and the column. `paths` keeps the search in the named folders and a \
                          leading `!` excludes one. The answer is JSON: `matches`, `total`, `more` and \
                          `timedOut`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "q": {"type": "string", "description": "A regular expression"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders to search, a leading ! excludes"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds; the answer keeps what was found by then"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["q"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SEARCH_SYMBOL,
            description: "The types, methods and fields whose name contains the query, with the place \
                          of each. `include_external` adds the symbols of the SDK and the libraries. \
                          The answer is JSON: `symbols`, `total` and `more`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "q": {"type": "string", "description": "A part of a name"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders to search, a leading ! excludes"},
                    "include_external": {"type": "boolean", "description": "The symbols of the SDK and the libraries, false by default"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["q"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: SKILL_SEARCH,
            description: "One search for every kind: files by a glob, text, a regular expression or a \
                          symbol. `mode` picks the kind; the other parameters are the ones of that \
                          search.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {"type": "string", "enum": ["file", "text", "regex", "symbol"]},
                    "q": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders to search, a leading ! excludes"},
                    "include_external": {"type": "boolean", "description": "Only for symbol"},
                    "includeExcluded": {"type": "boolean", "description": "Only for file"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds; the answer keeps what was found by then"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["mode", "q"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: EXECUTE_TERMINAL_COMMAND,
            description: "Runs a line in the terminal of the window and answers what it printed, its \
                          `exitCode` and whether it timed out. Asks the user first unless brave mode \
                          allows trusted projects. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "executeInShell": {"type": "boolean", "description": "Run through the shell; true by default"},
                    "reuseExistingTerminalWindow": {"type": "boolean"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds"},
                    "maxLinesCount": {"type": "integer", "minimum": 1, "maximum": 20000, "description": "2000 by default"},
                    "truncateMode": {"type": "string", "enum": ["start", "middle", "end", "none"], "description": "end by default"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: EXECUTE_TOOL,
            description: "Calls a tool of the IDE by name. `command` is the name, then its arguments \
                          as JSON or as `key=value` pairs: `get_file_problems {\"filePath\":\
                          \"src/Main.txt\"}` or `find_symbol query=Widget limit=5`. The tool keeps \
                          its own permission question.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The tool name and its arguments"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GET_REPOSITORIES,
            description: "The roots of the version control repositories of the project. The answer is \
                          JSON: `repositories` with `path`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: GIT_STATUS,
            description: "The changed files of the repositories with the code of the index side and \
                          of the working tree side, in the porcelain style, and the counts of the \
                          answer. The answer is JSON: `repositories`, each with `files`, `total`, \
                          `branch` and `more`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repositoryPathRelativeToProject": {"type": "string", "description": "Only this repository"},
                    "includeUntracked": {"type": "boolean", "description": "True by default"},
                    "includeIgnored": {"type": "boolean", "description": "False by default"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "Per repository, 200 by default"},
                    "projectPath": {"type": "string", "description": "The project of the window, when the agent knows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_STATUS,
            description: "The debug sessions of the window with their id, name, state and the \
                          configuration they run, and the one the window shows. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: XDEBUG_START,
            description: "Starts a stored run configuration, or a temporary one at a line of a file, \
                          under the debugger and answers its session. `timeout` waits for the start \
                          (60 000 ms by default) and `graceWaitMs` for a pause after it. Asks the user \
                          first unless the session is already allowed. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "configurationName": {"type": "string", "description": "From get_run_configurations"},
                    "filePath": {"type": "string", "description": "A temporary configuration at this file"},
                    "line": {"type": "integer", "minimum": 1, "description": "The place of the run mark, 1-based"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds to wait for the start, 60000 by default"},
                    "graceWaitMs": {"type": "integer", "minimum": 0, "description": "Milliseconds to wait for a pause, 2000 by default"},
                    "programArguments": {"type": "string", "description": "Only when the configuration allows it"},
                    "workingDirectory": {"type": "string", "description": "Only when the configuration allows it"},
                    "envs": {"type": "array", "items": {"type": "string"}, "description": "NAME=VALUE, only when the configuration allows it"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_CONTROL,
            description: "Moves a debug session: a step, resume, pause, stop, wait for a pause or read \
                          its events. `sessionId` selects the session: the only one when it is absent, \
                          an error when several run. A pause answers `frameValues`; the tails name the \
                          refused breakpoints and what the logpoints printed. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "action": {
                        "type": "string",
                        "enum": ["STEP_INTO", "STEP_OVER", "STEP_OUT", "RESUME", "PAUSE", "STOP", "WAIT_FOR_PAUSE", "DRAIN_EVENTS"]
                    },
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds to wait for a pause, 30000 by default"},
                    "eventsLimit": {"type": "integer", "minimum": 0, "maximum": 1000, "description": "How many console lines the answer carries, 100 by default"},
                    "clearEventsAfterRead": {"type": "boolean", "description": "Accepted; the console of the window keeps its lines"}
                },
                "required": ["action"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_RUN_TO_LINE,
            description: "Continues a stopped session to a line of a file with a temporary point, as \
                          Run to Cursor does, and answers `paused`, `stopped` or `timeout`. The answer \
                          is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "filePath": {"type": "string", "description": "A file of the project"},
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "timeout": {"type": "integer", "minimum": 0, "description": "Milliseconds to wait for the line, 30000 by default"}
                },
                "required": ["filePath", "line"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_THREADS,
            description: "The threads of a stopped or running session with their state, the current \
                          one and how many frames the stopped thread has. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "50 by default"},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_STACK,
            description: "The frames of one thread of a stopped session, with their file, line and \
                          whether the frame is the current one. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "threadId": {"description": "A thread id or the name the window shows; the current thread by default"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "200 by default"},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_FRAME_VALUES,
            description: "The values of a frame of a stopped session as a tree, with the children of \
                          each value down to `depth`. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "frameIndex": {"type": "integer", "minimum": 0, "description": "The top frame by default"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 5, "description": "0 by default"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_VALUE_BY_PATH,
            description: "One value of a frame by its path, such as `[\"items\",\"[0]\",\"name\"]`, \
                          with the children down to `depth`. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "frameIndex": {"type": "integer", "minimum": 0, "description": "The top frame by default"},
                    "path": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Names and [index] steps, such as [\"items\",\"[0]\",\"name\"]"
                    },
                    "depth": {"type": "integer", "minimum": 0, "maximum": 5, "description": "0 by default"}
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_EVALUATE,
            description: "Evaluates an expression in a stopped frame, as the console of the debugger \
                          does, and answers its value and the children down to `depth`. The expression \
                          may change the program, so the user is asked first. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "frameIndex": {"type": "integer", "minimum": 0, "description": "The top frame by default"},
                    "expression": {"type": "string"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 5, "description": "0 by default"}
                },
                "required": ["expression"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_SET_VARIABLE,
            description: "Gives a variable of a stopped frame a new value, through the requests of the \
                          adapter; an adapter that cannot set the value says why. The answer is JSON \
                          with `oldValue`, `newValue` and `applied`.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The id of the session; the only one by default"},
                    "frameIndex": {"type": "integer", "minimum": 0, "description": "The top frame by default"},
                    "path": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Names and [index] steps of the value"
                    },
                    "newValue": {"type": "string", "description": "An expression of the language"}
                },
                "required": ["path", "newValue"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_LIST_BREAKPOINTS,
            description: "Every breakpoint with its owner (`agent` or `user`), its line, its condition, \
                          whether it is a logpoint, whether it goes away at the first stop, its suspend \
                          policy and its hit count. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": {"type": "string", "description": "Only the points of this file; every file by default"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_SET_BREAKPOINT,
            description: "Sets a breakpoint, or edits the one `breakpointId` names, and answers its final \
                          state. The point of an agent is marked `agent` in the editor. `condition` of \
                          null clears the condition; `suspendPolicy` is `ALL` or `NONE` (`THREAD` is not \
                          supported); `isLogStack` and a `hitCount` are not supported. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "breakpointId": {"type": "string", "description": "The point to edit, from xdebug_list_breakpoints"},
                    "filePath": {"type": "string", "description": "Relative to the project root or absolute"},
                    "line": {"type": "integer", "minimum": 1, "description": "1-based"},
                    "condition": {"type": ["string", "null"], "description": "An expression; null clears it"},
                    "isLogMessage": {"type": "boolean", "description": "Print the condition instead of stopping"},
                    "isLogStack": {"type": "boolean"},
                    "temporary": {"type": "boolean", "description": "Gone at the first stop"},
                    "suspendPolicy": {"type": "string", "enum": ["ALL", "THREAD", "NONE"]},
                    "hitCount": {"type": "integer", "minimum": 1},
                    "enabled": {"type": "boolean"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: XDEBUG_REMOVE_BREAKPOINT,
            description: "Takes breakpoints away by id, by file and line, or by owner. The selectors \
                          combine, and one `owner` takes away every point of that owner. A point that \
                          is gone stays gone without an error. The answer is JSON with `removed`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "breakpointId": {"type": "string", "description": "From xdebug_list_breakpoints"},
                    "filePath": {"type": "string", "description": "Relative to the project root or absolute"},
                    "line": {"type": "integer", "minimum": 1},
                    "owner": {"type": "string", "enum": ["agent", "user"], "description": "agent by default"}
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_CONNECTIONS,
            description: "The connections of the Database window with their id, name, product, driver \
                          and address. The address never holds a password. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        },
        ToolSpec {
            name: DATABASE_TEST,
            description: "Tests a connection and answers whether it has a problem, the product version \
                          and what the attempt said. Asks the user first. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "From list_database_connections"}
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_CREATE,
            description: "Adds a connection to the Database window and answers it, with a check of the \
                          connection when `needToCheckDs` is on. Asks the user first unless brave mode \
                          allows trusted projects. The address never keeps a password. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "dbms": {"type": "string", "description": "H2, PostgreSQL, MySQL, MariaDB, SQLite, Oracle or SQL Server"},
                    "url": {"type": "string", "description": "The JDBC address of the driver"},
                    "needToCheckDs": {"type": "boolean", "description": "Check the connection after saving, true by default"}
                },
                "required": ["name", "dbms", "url"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_EDIT,
            description: "Changes the product or the address of a connection and answers it, with a check \
                          of the connection when `needToCheckDs` is on. The address never holds a \
                          password in an answer. Asks the user first. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "dbms": {"type": "string", "description": "H2, PostgreSQL, MySQL, MariaDB, SQLite, Oracle or SQL Server"},
                    "url": {"type": "string", "description": "The JDBC address of the driver"},
                    "needToCheckDs": {"type": "boolean", "description": "Check the connection after saving, true by default"}
                },
                "required": ["connectionId", "url"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_SCHEMAS,
            description: "The schemas of a connection, with the database when the driver named one. \
                          `selectedOnly` keeps the ones the window selected. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "selectedOnly": {"type": "boolean", "description": "Only the schemas the window selected"}
                },
                "required": ["connectionId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_INTROSPECT,
            description: "Reads the schema of a connection again, or the named database and schema, and \
                          answers how far the read has come. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "databaseName": {"type": "string"},
                    "schemaName": {"type": "string"}
                },
                "required": ["connectionId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_KINDS,
            description: "The kinds of objects a connection has, such as `TABLE` and `VIEW`, with the \
                          codes the other tools pass. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"}
                },
                "required": ["connectionId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_OBJECTS,
            description: "The tables and views of a connection, or of one schema, with their kind. The \
                          answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "schemaName": {"type": "string"},
                    "databaseName": {"type": "string"},
                    "kind": {"type": "string", "description": "TABLE or VIEW"}
                },
                "required": ["connectionId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_DESCRIBE,
            description: "What the driver says about one object: its columns with their types and keys, \
                          its indexes and its foreign keys. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "databaseName": {"type": "string"},
                    "schemaName": {"type": "string"},
                    "kind": {"type": "string", "description": "TABLE or VIEW"},
                    "objectName": {"type": "string"}
                },
                "required": ["connectionId", "objectName"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_EXECUTE,
            description: "Runs a statement on a connection of the Database window and answers its status, \
                          the rows as comma-separated text with a header and a `resultSetId` that \
                          `fetch_query_result` pages. Asks the user first. The answer is JSON.",
            class: ToolClass::Execute,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "queryText": {"type": "string"}
                },
                "required": ["connectionId", "queryText"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_FETCH,
            description: "The rows of a result of `execute_sql_query`, as comma-separated text, from the \
                          given offset. The answer is JSON with `rows` and `hasMore`.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "resultSetId": {"type": "string", "description": "From execute_sql_query"},
                    "offset": {"type": "integer", "minimum": 0, "description": "How many rows to skip"}
                },
                "required": ["resultSetId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_PREVIEW,
            description: "The first rows of a table, as comma-separated text with a header. The answer \
                          is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"},
                    "schemaName": {"type": "string"},
                    "databaseName": {"type": "string"},
                    "tableName": {"type": "string"},
                    "maxRowCount": {"type": "integer", "minimum": 1, "maximum": 10000, "description": "100 by default"}
                },
                "required": ["connectionId", "tableName"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_RECENT,
            description: "The statements the console of a connection ran or still runs, newest first, \
                          with their state. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connectionId": {"type": "string"}
                },
                "required": ["connectionId"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: DATABASE_CANCEL,
            description: "Stops the statement that runs on a connection. The id of the connection is the \
                          session of a statement in this window. The answer is JSON.",
            class: ToolClass::Read,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string", "description": "The connection id"}
                },
                "required": ["sessionId"],
                "additionalProperties": false,
            }),
        },
    ]
});

/// Every tool, with its name, description and schema, for the Exposed Tools table.
pub fn all_tools() -> &'static [ToolSpec] {
    &TOOLS
}

/// The group of the reference a tool belongs to, for the Exposed Tools table.
pub fn group_of(name: &str) -> &'static str {
    match name {
        ANALYZE_CALLS
        | BUILD_PROJECT
        | GET_FILE_PROBLEMS
        | GET_PROJECT_DEPENDENCIES
        | GET_PROJECT_MODULES
        | LINT_FILES
        | DIAGNOSTICS
        | PROJECT_STRUCTURE => "Analysis",
        EDITOR_CONTEXT | SYMBOL_INFO | FIND_SYMBOL | FIND_USAGES | TYPE_HIERARCHY
        | CALL_HIERARCHY => "Code Insight",
        GET_RUN_CONFIGURATIONS
        | EXECUTE_RUN_CONFIGURATION
        | RUN_TESTS
        | LIST_RUN_CONFIGURATIONS
        | RUN_CONFIGURATION
        | GET_RUN_OUTPUT
        | STOP_RUN => "Execution",
        CREATE_NEW_FILE | GET_ALL_OPEN_FILE_PATHS | LIST_DIRECTORY_TREE | OPEN_FILE_IN_EDITOR => {
            "Files"
        }
        REFORMAT_FILE | APPLY_PATCH | RENAME_REFACTORING | PROPOSE_EDIT | RENAME_SYMBOL
        | ORGANIZE_IMPORTS | FORMAT_FILE => "Editing",
        READ_FILE => "Read",
        SEARCH_FILE | SEARCH_REGEX | SEARCH_SYMBOL | SEARCH_TEXT | SKILL_SEARCH => "Search",
        EXECUTE_TERMINAL_COMMAND => "Terminal",
        EXECUTE_TOOL => "Universal",
        GET_REPOSITORIES | GIT_STATUS | VCS_CHANGES => "VCS",
        _ => "Debugger",
    }
}

#[cfg(test)]
mod tests {
    use super::{TOOL_GROUPS, TOOL_NAMES, TOOLS, ToolClass, group_of};

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
    fn every_tool_belongs_to_a_group_of_the_table() {
        for name in TOOL_NAMES {
            let group = group_of(name);
            assert!(
                TOOL_GROUPS.contains(&group),
                "{name} is in the unknown group {group}"
            );
        }
        assert_eq!(group_of("analyze_calls"), "Analysis");
        assert_eq!(group_of("execute_terminal_command"), "Terminal");
        assert_eq!(group_of("execute_tool"), "Universal");
        assert_eq!(group_of("debug_stack"), "Debugger");
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

    #[test]
    fn the_debug_tools_are_listed_and_only_the_program_ones_need_the_user() {
        for name in [
            "set_breakpoint",
            "remove_breakpoint",
            "list_breakpoints",
            "debug_run",
            "debug_wait",
            "debug_stack",
            "debug_variables",
            "debug_evaluate",
            "debug_step",
            "debug_stop",
        ] {
            let spec = TOOLS.iter().find(|spec| spec.name == name).expect(name);
            let runs_the_program = matches!(
                name,
                "debug_run" | "debug_evaluate" | "debug_step" | "debug_stop"
            );
            let class = if runs_the_program {
                ToolClass::Execute
            } else {
                ToolClass::Read
            };
            assert_eq!(spec.class, class, "{name}");
        }
    }
}
