//! The section an agent reads in `AGENTS.md`: which IDE tools exist and when they beat reading
//! files. Plain text search stays the default; agents pick semantics rarely on their own, and
//! forcing it made them worse in the pilot (section 7 of the architecture).

use crate::tools::{
    CALL_HIERARCHY, DEBUG_EVALUATE, DEBUG_RUN, DEBUG_STACK, DEBUG_STEP, DEBUG_STOP,
    DEBUG_VARIABLES, DEBUG_WAIT, DIAGNOSTICS, EDITOR_CONTEXT, FIND_SYMBOL, FIND_USAGES,
    FORMAT_FILE, LIST_BREAKPOINTS, ORGANIZE_IMPORTS, PROJECT_STRUCTURE, PROPOSE_EDIT, READ_FILE,
    REMOVE_BREAKPOINT, RENAME_SYMBOL, SEARCH_TEXT, SET_BREAKPOINT, SYMBOL_INFO, TOOLS,
    TYPE_HIERARCHY, VCS_CHANGES,
};

pub const HINT_START: &str = "<!-- ide-tools:start -->";
pub const HINT_END: &str = "<!-- ide-tools:end -->";

/// When to reach for each tool, in the words of a person and not of the schema.
fn when(name: &str) -> &'static str {
    match name {
        EDITOR_CONTEXT => "the user says \"this\", \"here\" or \"the open file\"",
        FIND_SYMBOL => "you know a name and want its declaration, not every mention of it",
        SYMBOL_INFO => "you need the type, signature or documentation of what is at a line",
        DIAGNOSTICS => "after an edit, to see compiler errors without a build",
        FIND_USAGES => "you need every use of a symbol, not only its declaration",
        TYPE_HIERARCHY => "you need the parents or the children of a type",
        CALL_HIERARCHY => "you need to know which methods call this one",
        PROJECT_STRUCTURE => "you need the modules, source roots or dependencies of the project",
        SEARCH_TEXT => "you look for a string or a comment inside the project",
        READ_FILE => "you need the text of a file, including edits the user has not saved",
        VCS_CHANGES => "you need what changed against the last commit, or the diff of one file",
        PROPOSE_EDIT => "the user wants to see an edit before it is written",
        RENAME_SYMBOL => "a declaration changes its name and every use should follow",
        ORGANIZE_IMPORTS => "the imports of a file need tidying after an edit",
        FORMAT_FILE => "the code of a file should match the formatting of the project",
        SET_BREAKPOINT => "you want the program to stop at a line while you watch it",
        REMOVE_BREAKPOINT => "a breakpoint you set is no longer needed",
        LIST_BREAKPOINTS => "you need to know which breakpoints the project has",
        DEBUG_RUN => "you want to watch a test or a program run instead of reading its log",
        DEBUG_WAIT => "the program is still running and you want its next stop",
        DEBUG_STACK => "you need the calls that led to the stopped line",
        DEBUG_VARIABLES => "you need the values of the stopped frame",
        DEBUG_EVALUATE => "you need a value the variables do not show, such as a getter",
        DEBUG_STEP => "you need the next line, the inside of a call or the end of a method",
        DEBUG_STOP => "the debug session should end",
        _ => "its description fits the question",
    }
}

/// The section, between markers so that it can be replaced later.
pub fn agents_md_section(server: &str) -> String {
    let mut text = format!("{HINT_START}\n## IDE tools\n\n");
    text.push_str(&format!(
        "The IDE runs an MCP server named `{server}`. Its answers come with the line itself \
         and two lines around it, so there is no need to open the file again.\n\n"
    ));
    for spec in TOOLS.iter() {
        text.push_str(&format!("- `{}`: when {}.\n", spec.name, when(spec.name)));
    }
    text.push_str(
        "\nReach for the IDE before the shell: `search_text` respects the exclusions of \
         the project and `read_file` shows unsaved edits. Edit a file with `propose_edit` \
         when the user wants to see the change before it is written; never rewrite the \
         text of a symbol to rename it, use `rename_symbol` so that every use follows. Keep \
         the shell for what these tools do not cover, such as running a program. An answer \
         marked `confidence: unknown` means the IDE could not tell, not that nothing exists.\n",
    );
    text.push_str(
        "\nA program that misbehaves is often better watched than printed into. Put a \
         breakpoint with `set_breakpoint` on the line where the state goes wrong, start the \
         test or the configuration with `debug_run`, read `debug_stack` and \
         `debug_variables`, and move with `debug_step`. Choose the debugger over adding \
         print calls and running again when the failure depends on values inside a call, on \
         one iteration of a loop, or on an exception whose stack alone does not explain the \
         state. Keep printing when the question is about the order of events over time or \
         about a program nobody watches. A debug session stops the program, so end it with \
         `debug_stop`, and leave the breakpoints of the user alone when they do not belong \
         to the question.\n",
    );
    text.push_str(HINT_END);
    text.push('\n');
    text
}

/// `AGENTS.md` with the section added, or put in place of an older one.
pub fn with_agents_md_section(existing: &str, server: &str) -> String {
    let section = agents_md_section(server);
    let start = existing.find(HINT_START);
    let end = existing.find(HINT_END).map(|end| end + HINT_END.len());
    match (start, end) {
        (Some(start), Some(end)) if start < end => {
            let tail = existing[end..]
                .strip_prefix('\n')
                .unwrap_or(&existing[end..]);
            format!("{}{section}{tail}", &existing[..start])
        }
        _ if existing.trim().is_empty() => section,
        _ => {
            let separator = if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{existing}{separator}{section}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TOOL_NAMES;

    #[test]
    fn the_section_names_every_tool() {
        let section = agents_md_section("javaide");

        for name in TOOL_NAMES {
            assert!(section.contains(&format!("`{name}`")), "{name}");
        }
        assert!(section.contains("`javaide`"));
    }

    #[test]
    fn the_section_is_added_once_and_then_replaced() {
        let first = with_agents_md_section("# Project\n", "javaide");
        let again = with_agents_md_section(&first, "javaide");

        assert_eq!(first, again);
        assert!(first.starts_with("# Project\n\n<!-- ide-tools:start -->"));
        let old = format!("a\n{HINT_START}\nold\n{HINT_END}\nb\n");
        let replaced = with_agents_md_section(&old, "javaide");
        assert!(replaced.starts_with("a\n<!-- ide-tools:start -->"));
        assert!(replaced.ends_with("<!-- ide-tools:end -->\nb\n"));
        assert!(!replaced.contains("old"));
    }
}
