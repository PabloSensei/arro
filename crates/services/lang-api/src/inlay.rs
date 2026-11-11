//! Hints a language puts between the characters of a line. They are not part of the
//! text: the caret, a selection and a copy never see them.

use crate::Provider;
use crate::{TextPoint, TextSpan};

/// What a hint tells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlayHintKind {
    /// The name of a parameter before its argument.
    #[default]
    Parameter,
    /// The type of a declaration, a return or a parameter.
    Type,
    /// Something the editor shows when it knows nothing better.
    Other,
}

/// One hint at the place it belongs. Positions are zero-based and count UTF-16 units.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlayHintEntry {
    /// Where the hint is drawn: before the argument or after the declaration.
    pub position: TextPoint,
    pub label: String,
    pub kind: InlayHintKind,
    /// The language asks for a gap before the label.
    pub padding_left: bool,
    /// The language asks for a gap after the label.
    pub padding_right: bool,
}

/// The part of a document the editor shows, with the room to scroll into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHintQuery {
    pub uri: String,
    pub version: i32,
    pub range: TextSpan,
}

/// Hints for the part of a document the editor shows.
pub type InlayHintProvider = dyn Provider<InlayHintQuery, Vec<InlayHintEntry>>;

/// Which parameter names a language shows, as Settings names the choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlayParameterNames {
    /// No names at all.
    None,
    /// Only before a literal or another argument whose meaning the name explains.
    #[default]
    Literals,
    /// Before every argument.
    All,
}

/// The Inlay Hints settings of the editor. The editor filters the kinds it shows; a
/// language also tells its server, so that the server can narrow the answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlayHintSettings {
    pub parameter_names: InlayParameterNames,
    pub type_hints: bool,
}
