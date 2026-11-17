//! The sequence diagram of one method: the participants, the messages between them in
//! the order the method makes them, and the fragments that group messages under a
//! condition. A language reads it from its sources in the background.

use std::ops::Range;

use crate::{DocumentSpot, Provider, SourceSpan};

/// What a participant of the diagram is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceParticipantKind {
    Class,
    Interface,
    /// A type the project does not declare, such as a library type.
    External,
    Actor,
}

/// One participant of the diagram, a class or an external type with its line of life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceParticipant {
    pub name: String,
    /// The name with its package where the language knows one.
    pub qualified: String,
    pub kind: SequenceParticipantKind,
    /// The declaration of the participant, for a double click.
    pub place: Option<SourceSpan>,
}

/// How a message is drawn and read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceMessageKind {
    /// A call from one participant to another.
    Call,
    /// A call a participant makes on itself.
    SelfCall,
    /// An object of the target type is created.
    Creation,
    /// A value a call hands back to its caller.
    Return,
}

/// One message of the diagram, in the order of the method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceMessage {
    pub from: usize,
    pub to: usize,
    pub kind: SequenceMessageKind,
    /// The method with its arguments, `findById(id)`, or the type a call returns.
    pub label: String,
    /// The type the call returns, drawn on the return arrow.
    pub returns: Option<String>,
    /// The place of the call in its source, for a double click.
    pub place: Option<SourceSpan>,
    /// The fragments that contain the message, from the outermost one.
    pub fragments: Vec<usize>,
    /// How deep the call is: one for a call of the method itself.
    pub depth: usize,
}

/// What a fragment groups its messages under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceFragmentKind {
    /// An `if` with its `else`, or the branches of a `switch`.
    Alt,
    /// An `if` without an `else`.
    Opt,
    /// A `for`, a `while`, a `do` or a `forEach` of a lambda.
    Loop,
}

/// One branch of a fragment with the condition it runs under and the messages it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceSection {
    pub condition: String,
    /// The messages of the section, from the first one to the one after the last.
    pub messages: Range<usize>,
}

/// One frame of the diagram: an `alt`, an `opt` or a `loop` with its branches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceFragment {
    pub kind: SequenceFragmentKind,
    pub sections: Vec<SequenceSection>,
    /// How many frames contain this one.
    pub depth: usize,
}

/// The participants, the messages and the fragments of one sequence diagram.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceDiagram {
    pub participants: Vec<SequenceParticipant>,
    pub messages: Vec<SequenceMessage>,
    pub fragments: Vec<SequenceFragment>,
    /// The method the diagram starts at, such as `OwnerController.processFindForm(page)`.
    pub title: String,
}

/// What the build of a sequence diagram leaves out and how deep it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceSettings {
    /// How many levels of calls below the method are expanded.
    pub depth: usize,
    /// Leaves out calls to getters and setters.
    pub skip_accessors: bool,
    /// Leaves out calls to private methods.
    pub skip_private: bool,
    /// Leaves out the creation of objects.
    pub skip_constructors: bool,
    /// Leaves out the calls of types the project does not declare.
    pub project_only: bool,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            depth: 3,
            skip_accessors: false,
            skip_private: false,
            skip_constructors: false,
            project_only: false,
        }
    }
}

/// The method at a place and the settings of its diagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceQuery {
    pub spot: DocumentSpot,
    pub settings: SequenceSettings,
}

pub type SequenceDiagramProvider = dyn Provider<SequenceQuery, SequenceDiagram>;
