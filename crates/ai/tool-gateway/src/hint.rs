//! The section an agent reads in `AGENTS.md`: which IDE tools exist and when they beat reading
//! files. Plain text search stays the default; agents pick semantics rarely on their own, and
//! forcing it made them worse in the pilot (section 7 of the architecture).

use crate::tools::{DIAGNOSTICS, EDITOR_CONTEXT, FIND_SYMBOL, SYMBOL_INFO, TOOLS};

pub const HINT_START: &str = "<!-- ide-tools:start -->";
pub const HINT_END: &str = "<!-- ide-tools:end -->";

/// When to reach for each tool, in the words of a person and not of the schema.
fn when(name: &str) -> &'static str {
    match name {
        EDITOR_CONTEXT => "the user says \"this\", \"here\" or \"the open file\"",
        FIND_SYMBOL => "you know a name and want its declaration, not every mention of it",
        SYMBOL_INFO => "you need the type, signature or documentation of what is at a line",
        DIAGNOSTICS => "after an edit, to see compiler errors without a build",
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
        "\nKeep using text search for strings, comments and anything that is not a symbol. \
         An answer marked `confidence: unknown` means the IDE could not tell, not that nothing \
         exists.\n",
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
