//! The lines between the window and a console helper, in both directions.
//!
//! One request or answer is one JSON object on one line. The helper runs the fragments of
//! a console in a process of its own and answers each of them; the text a fragment prints
//! comes as its own message, so the window shows it as it arrives.

use serde_json::{Map, Value as Json, json};

/// The version of the protocol both sides speak.
pub const PROTOCOL: u32 = 1;

/// The most characters of a value the helper sends. A longer one is cut and marked.
pub const MAX_VALUE_CHARS: usize = 10_000;

/// A request of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Runs a text that may hold several fragments; the helper splits it itself.
    Eval {
        id: u64,
        text: String,
    },
    /// Drops every name and starts an empty console.
    Reset {
        id: u64,
    },
    /// Interrupts the fragment that runs now, when one does.
    Stop,
    /// Asks for the continuations of `text` at `cursor`.
    Complete {
        id: u64,
        text: String,
        cursor: usize,
    },
    Close,
}

impl Command {
    /// The line the helper reads. The caller writes the newline.
    pub fn encode(&self) -> String {
        let mut object = Map::new();
        match self {
            Self::Eval { id, text } => {
                object.insert("type".into(), json!("eval"));
                object.insert("id".into(), json!(id));
                object.insert("text".into(), json!(text));
            }
            Self::Reset { id } => {
                object.insert("type".into(), json!("reset"));
                object.insert("id".into(), json!(id));
            }
            Self::Stop => {
                object.insert("type".into(), json!("stop"));
            }
            Self::Complete { id, text, cursor } => {
                object.insert("type".into(), json!("complete"));
                object.insert("id".into(), json!(id));
                object.insert("text".into(), json!(text));
                object.insert("cursor".into(), json!(cursor));
            }
            Self::Close => {
                object.insert("type".into(), json!("close"));
            }
        }
        Json::Object(object).to_string()
    }
}

/// What a fragment turned out to be. The window writes the word next to the result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FragmentKind {
    Expression,
    Variable,
    Method,
    Class,
    Import,
    Statement,
    Error,
    #[default]
    Other,
}

impl FragmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Variable => "variable",
            Self::Method => "method",
            Self::Class => "class",
            Self::Import => "import",
            Self::Statement => "statement",
            Self::Error => "error",
            Self::Other => "other",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "expression" => Self::Expression,
            "variable" => Self::Variable,
            "method" => Self::Method,
            "class" => Self::Class,
            "import" => Self::Import,
            "statement" => Self::Statement,
            "error" => Self::Error,
            _ => Self::Other,
        }
    }
}

/// One problem a fragment has, with the place in the fragment text it points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostic {
    /// Byte offsets in the fragment source; equal when the place has no width.
    pub start: usize,
    pub end: usize,
    /// One-based, as the window writes them.
    pub line: u32,
    pub column: u32,
    pub message: String,
    /// The code the compiler gave, empty when there is none.
    pub code: String,
}

/// What a fragment threw, with the stack the helper read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exception {
    pub message: String,
    pub stack: String,
}

/// One continuation the helper offers at the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    /// The text that replaces the typed word, such as `substring(`.
    pub text: String,
    /// Whether the continuation fits the expected type, when the helper knows one.
    pub matches_type: bool,
}

/// What the helper says.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Ready {
        protocol: u32,
        runtime: String,
        engine: String,
    },
    /// One fragment of an `eval` request has run.
    Fragment {
        id: u64,
        /// One-based number inside the request, the way the console shows `$1`, `$2`.
        index: u32,
        kind: FragmentKind,
        /// The name the helper gave the fragment, such as `x` or `$1`.
        name: String,
        value: Option<String>,
        type_name: Option<String>,
        /// `VALID`, `REJECTED` and the rest of the helper's words.
        status: String,
        /// The declaration without its body, for a method or a class.
        signature: Option<String>,
        /// The text the helper ran for this fragment.
        source: String,
        diagnostics: Vec<Diagnostic>,
        exception: Option<Exception>,
        /// The value was longer than [`MAX_VALUE_CHARS`] and was cut.
        truncated: bool,
    },
    /// Text a fragment printed. A fragment may print many times.
    Output {
        id: u64,
        text: String,
        /// True for the error stream of the fragment.
        error: bool,
    },
    /// The request is over: every fragment has run or the run was interrupted.
    Done {
        id: u64,
        cancelled: bool,
    },
    Completion {
        id: u64,
        items: Vec<CompletionItem>,
    },
    /// The request could not run at all.
    Error {
        id: u64,
        message: String,
    },
    /// The helper ended: the console is over and has to be started again.
    Exited {
        code: Option<i32>,
    },
}

impl Event {
    /// A line that came from the helper, when it is one of the known answers.
    pub fn decode(line: &str) -> Option<Self> {
        let value: Json = serde_json::from_str(line).ok()?;
        let object = value.as_object()?;
        let kind = object.get("type")?.as_str()?;
        let id = object.get("id").and_then(Json::as_u64).unwrap_or(0);
        Some(match kind {
            "ready" => Self::Ready {
                protocol: object.get("protocol").and_then(Json::as_u64).unwrap_or(0) as u32,
                runtime: text(object, "runtime"),
                engine: text(object, "engine"),
            },
            "fragment" => Self::Fragment {
                id,
                index: object.get("index").and_then(Json::as_u64).unwrap_or(0) as u32,
                kind: FragmentKind::parse(
                    object
                        .get("kind")
                        .and_then(Json::as_str)
                        .unwrap_or_default(),
                ),
                name: text(object, "name"),
                value: optional_text(object, "value"),
                type_name: optional_text(object, "valueType"),
                status: text(object, "status"),
                signature: optional_text(object, "signature"),
                source: text(object, "source"),
                diagnostics: object
                    .get("diagnostics")
                    .and_then(Json::as_array)
                    .map(|values| values.iter().filter_map(diagnostic).collect())
                    .unwrap_or_default(),
                exception: object.get("exception").and_then(exception),
                truncated: object
                    .get("truncated")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            },
            "output" => Self::Output {
                id,
                text: text(object, "text"),
                error: object
                    .get("stream")
                    .and_then(Json::as_str)
                    .is_some_and(|stream| stream == "err"),
            },
            "done" => Self::Done {
                id,
                cancelled: object
                    .get("cancelled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            },
            "completion" => Self::Completion {
                id,
                items: object
                    .get("items")
                    .and_then(Json::as_array)
                    .map(|values| values.iter().filter_map(completion_item).collect())
                    .unwrap_or_default(),
            },
            "error" => Self::Error {
                id,
                message: text(object, "message"),
            },
            "exited" => Self::Exited {
                code: object
                    .get("code")
                    .and_then(Json::as_i64)
                    .map(|code| code as i32),
            },
            _ => return None,
        })
    }
}

fn text(object: &Map<String, Json>, key: &str) -> String {
    object
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn optional_text(object: &Map<String, Json>, key: &str) -> Option<String> {
    object.get(key).and_then(Json::as_str).map(str::to_owned)
}

fn diagnostic(value: &Json) -> Option<Diagnostic> {
    let object = value.as_object()?;
    Some(Diagnostic {
        start: object.get("start").and_then(Json::as_u64).unwrap_or(0) as usize,
        end: object.get("end").and_then(Json::as_u64).unwrap_or(0) as usize,
        line: object.get("line").and_then(Json::as_u64).unwrap_or(0) as u32,
        column: object.get("column").and_then(Json::as_u64).unwrap_or(0) as u32,
        message: text(object, "message"),
        code: text(object, "code"),
    })
}

fn exception(value: &Json) -> Option<Exception> {
    let object = value.as_object()?;
    Some(Exception {
        message: text(object, "message"),
        stack: text(object, "stack"),
    })
}

fn completion_item(value: &Json) -> Option<CompletionItem> {
    let object = value.as_object()?;
    Some(CompletionItem {
        text: text(object, "text"),
        matches_type: object
            .get("matchesType")
            .and_then(Json::as_bool)
            .unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_line_of_json() {
        let line = Command::Eval {
            id: 7,
            text: "int x = 2;".into(),
        }
        .encode();
        let value: Json = serde_json::from_str(&line).unwrap();

        assert_eq!(value["type"], "eval");
        assert_eq!(value["id"], 7);
        assert_eq!(value["text"], "int x = 2;");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_fragment_keeps_its_place_and_its_exception() {
        let event = Event::decode(
            r#"{"type":"fragment","id":3,"index":2,"kind":"error","name":"1",
                "status":"REJECTED","source":"int y = ;","diagnostics":[
                {"start":8,"end":9,"line":1,"column":9,"message":"illegal start","code":"compiler.err"}],
                "exception":{"message":"boom","stack":"at REPL.$JShell$1"}}"#,
        )
        .expect("a fragment");

        let Event::Fragment {
            kind,
            diagnostics,
            exception,
            ..
        } = event
        else {
            panic!("not a fragment");
        };
        assert_eq!(kind, FragmentKind::Error);
        assert_eq!(diagnostics[0].column, 9);
        assert_eq!(diagnostics[0].message, "illegal start");
        assert_eq!(exception.unwrap().message, "boom");
    }

    #[test]
    fn the_output_of_a_fragment_names_its_stream() {
        let event = Event::decode(r#"{"type":"output","id":1,"stream":"err","text":"bad"}"#)
            .expect("output");
        assert_eq!(
            event,
            Event::Output {
                id: 1,
                text: "bad".into(),
                error: true
            }
        );
    }
}
