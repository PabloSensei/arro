//! The script of a mock agent, read from a file: what a demo plays in the chat without a
//! model. A transcript is a list of turns, or an object with the turns and a `title`; a turn
//! is a list of steps, and the window shows every step as it shows the same event of a real
//! harness. The title names the session of the transcript, as "Claude Agent 1".

use std::path::{Path, PathBuf};

use agent_core::MockStep;
use serde_json::{Value, json};

/// The variable that names a transcript, for a demo run that has no window script.
pub(crate) const VARIABLE: &str = "ARRO_MOCK_TRANSCRIPT";

/// What a transcript plays: its turns, and the title that names the session when the file
/// gives one.
pub(crate) struct Script {
    pub(crate) title: Option<String>,
    pub(crate) turns: Vec<Vec<MockStep>>,
}

/// The script of the mock session: the transcript a window script names or the one in
/// `ARRO_MOCK_TRANSCRIPT`, and `None` when neither names a file. A relative path belongs
/// to the project, as the paths of the other script steps do.
pub(crate) fn named(script: Option<&Path>, root: &Path) -> Result<Option<Script>, String> {
    let chosen = script
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(VARIABLE).map(PathBuf::from));
    let Some(chosen) = chosen else {
        return Ok(None);
    };
    let path = if chosen.is_absolute() {
        chosen
    } else {
        root.join(chosen)
    };
    load(&path).map(Some)
}

/// Reads one transcript file.
pub(crate) fn load(path: &Path) -> Result<Script, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("the transcript {} is not read: {error}", path.display()))?;
    parse(&text).map_err(|error| format!("the transcript {}: {error}", path.display()))
}

/// Reads the text of a transcript, in either form.
pub(crate) fn parse(text: &str) -> Result<Script, String> {
    const SHAPE: &str = "a transcript is a list of turns, or an object with \"turns\"";
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("it is not JSON: {error}"))?;
    let (title, turns) = match &value {
        Value::Array(turns) => (None, turns),
        Value::Object(object) => {
            let turns = object.get("turns").and_then(Value::as_array).ok_or(SHAPE)?;
            let title = match object.get("title") {
                None => None,
                Some(Value::String(title)) => Some(title.clone()),
                Some(_) => return Err("\"title\" is a string".to_string()),
            };
            (title, turns)
        }
        _ => return Err(SHAPE.to_string()),
    };
    Ok(Script {
        title,
        turns: parse_turns(turns)?,
    })
}

/// The turns of a transcript: each one a list of steps, and a user message only opens a turn.
fn parse_turns(turns: &[Value]) -> Result<Vec<Vec<MockStep>>, String> {
    let mut turns_out = Vec::with_capacity(turns.len());
    for (turn_index, turn) in turns.iter().enumerate() {
        let at = format!("turn {}", turn_index + 1);
        let steps = turn
            .as_array()
            .ok_or_else(|| format!("{at}: a turn is a list of steps"))?;
        let mut steps_out = Vec::with_capacity(steps.len());
        for (step_index, step) in steps.iter().enumerate() {
            let at = format!("{at}, step {}", step_index + 1);
            let step = parse_step(step).map_err(|error| format!("{at}: {error}"))?;
            if step_index > 0 && matches!(step, MockStep::User(_)) {
                return Err(format!("{at}: a user message opens a turn"));
            }
            steps_out.push(step);
        }
        turns_out.push(steps_out);
    }
    Ok(turns_out)
}

/// The keys that name the kind of a step. `input` belongs to `tool`.
const KINDS: &[&str] = &["reply", "tool", "permission", "edit", "user"];

fn parse_step(value: &Value) -> Result<MockStep, String> {
    let object = value.as_object().ok_or("a step is an object")?;
    let mut kind: Option<(&str, &Value)> = None;
    for (key, value) in object {
        if key == "input" {
            continue;
        }
        if !KINDS.contains(&key.as_str()) {
            return Err(format!(
                "unknown step {key:?}, use \"reply\", \"tool\", \"permission\", \"edit\" or \"user\""
            ));
        }
        if kind.is_some() {
            return Err("a step has one kind".to_string());
        }
        kind = Some((key, value));
    }
    let Some((kind, value)) = kind else {
        return Err(
            "a step needs \"reply\", \"tool\", \"permission\", \"edit\" or \"user\"".to_string(),
        );
    };
    match kind {
        "reply" => Ok(MockStep::Reply(text(value, "reply")?)),
        "user" => Ok(MockStep::User(text(value, "user")?)),
        "tool" => {
            let name = text(value, "tool")?;
            let input = match object.get("input") {
                Some(input) if input.is_object() => input.clone(),
                Some(_) => return Err("\"input\" is an object".to_string()),
                None => json!({}),
            };
            Ok(MockStep::Tool { name, input })
        }
        "permission" => {
            let permission = value.as_object().ok_or("\"permission\" is an object")?;
            Ok(MockStep::Permission {
                tool: text(
                    permission
                        .get("tool")
                        .ok_or("a permission needs \"tool\"")?,
                    "permission.tool",
                )?,
                description: text(
                    permission
                        .get("description")
                        .ok_or("a permission needs \"description\"")?,
                    "permission.description",
                )?,
            })
        }
        "edit" => {
            let edit = value.as_object().ok_or("\"edit\" is an object")?;
            let old_text = text(
                edit.get("old_text").ok_or("an edit needs \"old_text\"")?,
                "edit.old_text",
            )?;
            if old_text.is_empty() {
                return Err("\"edit.old_text\" is not empty: an edit replaces text".to_string());
            }
            Ok(MockStep::Edit {
                path: text(
                    edit.get("path").ok_or("an edit needs \"path\"")?,
                    "edit.path",
                )?
                .into(),
                old_text,
                new_text: text(
                    edit.get("new_text").ok_or("an edit needs \"new_text\"")?,
                    "edit.new_text",
                )?,
            })
        }
        other => Err(format!("unknown step {other:?}")),
    }
}

fn text(value: &Value, what: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("\"{what}\" is a string"))
}

#[cfg(test)]
mod tests {
    use super::{load, parse};

    /// The error of a transcript that must not parse.
    fn error_of(text: &str) -> String {
        match parse(text) {
            Ok(_) => panic!("{text} parses"),
            Err(error) => error,
        }
    }

    #[test]
    fn reads_every_kind_of_step() {
        let turns = parse(
            r#"[
                [
                    {"user": "Where does the search page get its owners?"},
                    {"reply": "Let me look."},
                    {"tool": "find_usages", "input": {"name": "OwnerRepository"}},
                    {"tool": "read_file"},
                    {"permission": {"tool": "Edit", "description": "edit src/Main.txt"}},
                    {"edit": {"path": "src/Main.txt", "old_text": "a", "new_text": "b"}}
                ],
                [{"reply": "Done."}]
            ]"#,
        )
        .expect("the transcript parses")
        .turns;
        assert_eq!(turns.len(), 2);
        let steps = &turns[0];
        assert!(
            matches!(&steps[0], agent_core::MockStep::User(text) if text == "Where does the search page get its owners?")
        );
        assert!(matches!(&steps[1], agent_core::MockStep::Reply(text) if text == "Let me look."));
        assert!(matches!(
            &steps[2],
            agent_core::MockStep::Tool { name, input }
                if name == "find_usages" && input["name"] == "OwnerRepository"
        ));
        assert!(matches!(
            &steps[3],
            agent_core::MockStep::Tool { name, input }
                if name == "read_file" && input.as_object().is_some_and(|object| object.is_empty())
        ));
        assert!(matches!(
            &steps[4],
            agent_core::MockStep::Permission { tool, description }
                if tool == "Edit" && description == "edit src/Main.txt"
        ));
        assert!(matches!(
            &steps[5],
            agent_core::MockStep::Edit { path, old_text, new_text }
                if path == std::path::Path::new("src/Main.txt")
                    && old_text == "a"
                    && new_text == "b"
        ));
        assert!(matches!(
            &turns[1][0],
            agent_core::MockStep::Reply(text) if text == "Done."
        ));
    }

    #[test]
    fn a_bad_step_names_the_turn_and_the_step() {
        let cases = [
            ("{}", "a transcript is a list of turns"),
            ("[{}]", "turn 1: a turn is a list of steps"),
            ("[[1]]", "turn 1, step 1: a step is an object"),
            (
                "[[{\"raply\": \"x\"}]]",
                "turn 1, step 1: unknown step \"raply\"",
            ),
            ("[[{}]]", "turn 1, step 1: a step needs"),
            (
                "[[{\"reply\": 1}]]",
                "turn 1, step 1: \"reply\" is a string",
            ),
            (
                "[[{\"reply\": \"a\", \"tool\": \"b\"}]]",
                "turn 1, step 1: a step has one kind",
            ),
            (
                "[[{\"tool\": \"find_usages\", \"input\": []}]]",
                "turn 1, step 1: \"input\" is an object",
            ),
            (
                "[[{\"permission\": {\"tool\": \"Edit\"}}]]",
                "a permission needs \"description\"",
            ),
            (
                "[[{\"edit\": {\"path\": \"a\", \"new_text\": \"b\"}}]]",
                "an edit needs \"old_text\"",
            ),
            (
                "[[{\"edit\": {\"path\": \"a\", \"old_text\": \"\", \"new_text\": \"b\"}}]]",
                "an edit replaces text",
            ),
            (
                "[[{\"reply\": \"a\"}, {\"user\": \"late\"}]]",
                "turn 1, step 2: a user message opens a turn",
            ),
        ];
        for (text, expected) in cases {
            let error = error_of(text);
            assert!(error.contains(expected), "{text}: {error}");
        }
    }

    #[test]
    fn the_list_form_plays_its_turns_without_a_title() {
        let script = parse(r#"[[{"reply": "a"}], [{"reply": "b"}]]"#).expect("parses");
        assert_eq!(script.title, None);
        assert_eq!(script.turns.len(), 2);
    }

    #[test]
    fn the_object_form_names_the_session_and_plays_its_turns() {
        let script =
            parse(r#"{"title": "Claude Agent", "turns": [[{"reply": "a"}], [{"reply": "b"}]]}"#)
                .expect("parses");
        assert_eq!(script.title.as_deref(), Some("Claude Agent"));
        assert_eq!(script.turns.len(), 2);
        // The title is optional in the object form, and the turns are still checked.
        let untitled = parse(r#"{"turns": [[{"reply": "a"}]]}"#).expect("parses");
        assert_eq!(untitled.title, None);
        assert!(error_of(r#"{"title": "Claude Agent", "turns": [{}]}"#).contains("turn 1"));
        assert!(error_of(r#"{"title": 3, "turns": []}"#).contains("\"title\" is a string"));
        assert!(error_of(r#"{"title": "Claude Agent"}"#).contains("a transcript is a list"));
    }

    #[test]
    fn a_missing_file_is_named() {
        let path = test_support::scratch("mock-transcript-missing").join("absent.json");
        let error = load(&path).err().expect("the file is not there");
        assert!(error.contains("absent.json"), "{error}");
        assert!(error.contains("is not read"), "{error}");
    }

    #[test]
    fn the_petclinic_demo_transcript_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tools/demo/petclinic-agent.json");
        let script = load(&path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(script.title.as_deref(), Some("Claude Agent"));
        let turns = script.turns;
        assert_eq!(turns.len(), 1, "the demo shows one turn");
        let steps = &turns[0];
        assert!(steps.len() >= 10, "the demo shows the whole turn");
        assert!(matches!(&steps[0], agent_core::MockStep::User(_)));
        for tool in [
            "find_usages",
            "get_type_hierarchy",
            "get_diagnostics",
            "run_tests",
        ] {
            assert!(
                steps.iter().any(|step| {
                    matches!(step, agent_core::MockStep::Tool { name, .. } if name == tool)
                }),
                "the demo calls {tool}"
            );
        }
        assert!(steps.iter().any(|step| matches!(
            step,
            agent_core::MockStep::Permission { tool, .. } if tool == "run_tests"
        )));
        assert_eq!(
            steps
                .iter()
                .filter(|step| matches!(step, agent_core::MockStep::Edit { .. }))
                .count(),
            2,
            "the demo proposes the filter and its signature"
        );
    }
}
