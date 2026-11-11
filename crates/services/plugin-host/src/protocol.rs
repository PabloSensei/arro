//! What a plugin may ask of the IDE, protocol version 1. See docs/PLUGINS.md.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::{FAILED, INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, Responder};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    #[default]
    Info,
    Warning,
    Error,
}

/// A call the window answers: it owns the editors and the documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdeCall {
    /// Answer with [`ActiveDocument`] or `null` when no editor is active.
    ActiveDocument,
    /// Replace the primary selection, or insert at the caret when nothing is selected.
    /// One undo step, the way a typed edit is.
    ReplaceSelection {
        text: String,
    },
    /// Insert at the caret, leaving the selection out of it.
    InsertText {
        text: String,
    },
    OpenFile {
        path: PathBuf,
        line: Option<u32>,
    },
    /// In the status bar, or as a notification that stays until closed.
    ShowMessage {
        message: String,
        kind: MessageKind,
        notification: bool,
    },
}

/// The active editor, offsets in Unicode code points the way most languages index text.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ActiveDocument {
    pub path: Option<PathBuf>,
    pub text: String,
    pub selection: Selection,
    #[serde(rename = "selectedText")]
    pub selected_text: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl ActiveDocument {
    /// `selection` in bytes of `text`, as the editor keeps it.
    pub fn new(path: Option<PathBuf>, text: String, selection: std::ops::Range<usize>) -> Self {
        let points = |byte: usize| text[..byte].chars().count();
        let selected_text = text[selection.clone()].to_owned();
        let selection = Selection {
            start: points(selection.start),
            end: points(selection.end),
        };
        Self {
            path,
            text,
            selection,
            selected_text,
        }
    }
}

/// A call of one plugin, waiting for the window. Answer it once, from any thread.
#[derive(Debug)]
pub struct HostRequest {
    /// The plugin id.
    pub plugin: String,
    pub call: IdeCall,
    responder: Responder,
}

impl HostRequest {
    pub fn done(self) {
        self.responder.ok(Value::Null);
    }

    pub fn answer(self, result: impl Serialize) {
        match serde_json::to_value(result) {
            Ok(value) => self.responder.ok(value),
            Err(error) => self.responder.error(FAILED, error.to_string()),
        }
    }

    pub fn fail(self, message: impl Into<String>) {
        self.responder.error(FAILED, message);
    }
}

#[derive(Deserialize)]
struct TextParams {
    text: String,
}

#[derive(Deserialize)]
struct OpenParams {
    path: PathBuf,
    #[serde(default)]
    line: Option<u32>,
}

#[derive(Deserialize)]
struct MessageParams {
    message: String,
    #[serde(default)]
    kind: MessageKind,
    #[serde(default)]
    notification: bool,
}

#[derive(Deserialize)]
struct LogParams {
    message: String,
}

/// What became of one message from a plugin.
pub(crate) enum Routed {
    ToWindow(HostRequest),
    Log(String),
    /// Answered here already.
    Handled,
}

pub(crate) fn route(plugin: &str, incoming: Incoming) -> Routed {
    let Incoming {
        method,
        params,
        responder,
    } = incoming;
    if method == "log" {
        return match serde_json::from_value::<LogParams>(params) {
            Ok(params) => Routed::Log(params.message),
            Err(error) => Routed::Log(format!("a bad log message: {error}")),
        };
    }
    // Notifications other than the log mean nothing to this version.
    let Some(responder) = responder else {
        return Routed::Handled;
    };
    let call = match method.as_str() {
        "editor/activeDocument" => Ok(IdeCall::ActiveDocument),
        "editor/replaceSelection" => parse::<TextParams>(params)
            .map(|params| IdeCall::ReplaceSelection { text: params.text }),
        "editor/insertText" => {
            parse::<TextParams>(params).map(|params| IdeCall::InsertText { text: params.text })
        }
        "workspace/openFile" => parse::<OpenParams>(params).map(|params| IdeCall::OpenFile {
            path: params.path,
            line: params.line,
        }),
        "window/showMessage" => parse::<MessageParams>(params).map(|params| IdeCall::ShowMessage {
            message: params.message,
            kind: params.kind,
            notification: params.notification,
        }),
        _ => {
            responder.error(METHOD_NOT_FOUND, format!("no method \"{method}\" in api 1"));
            return Routed::Handled;
        }
    };
    match call {
        Ok(call) => Routed::ToWindow(HostRequest {
            plugin: plugin.to_owned(),
            call,
            responder,
        }),
        Err(error) => {
            responder.error(INVALID_PARAMS, format!("{method}: {error}"));
            Routed::Handled
        }
    }
}

fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_code_points() {
        let text = "ёж и ёлка".to_owned();
        let byte = text.rfind('ё').unwrap();

        let document = ActiveDocument::new(None, text, byte..byte + "ёлка".len());

        assert_eq!((document.selection.start, document.selection.end), (5, 9));
        assert_eq!(document.selected_text, "ёлка");
    }
}
