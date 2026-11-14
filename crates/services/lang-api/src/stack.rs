//! The stacks a program leaves in text: the frames of a stack trace and the threads of a
//! dump, and the places the language finds for the frames. The tool window shows the
//! report as it is; what a line means is the language's answer.

use crate::{Provider, SourceSpan};

/// How the code of a frame relates to the open project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameOrigin {
    /// A class of the project: the usual colour.
    Project,
    /// A class of a library or of the runtime: grey.
    Library,
    /// The language found nothing: grey, without a link.
    #[default]
    Unknown,
}

/// What a line of a report is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// `at pkg.Cls.method(File.ext:42)`.
    Frame,
    /// A note under a frame, such as `- locked <0x…>`.
    Lock,
    /// `Caused by:`, `Suppressed:` and `... N more`.
    Structure,
    /// A message, a header, a blank line.
    Text,
}

/// One frame of a stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The class the frame names, `pkg.Cls`, without the module prefix.
    pub class: String,
    pub method: String,
    /// The source file the frame names, `Main.ext`, when it named one.
    pub file: Option<String>,
    /// The line in the source the frame names, counting from zero.
    pub line: Option<usize>,
    /// Where the class is, filled by the language. `None` when it is not found or the
    /// frame names no line.
    pub place: Option<SourceSpan>,
    pub origin: FrameOrigin,
}

/// One line of a stack trace or of a thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line as the program wrote it.
    pub text: String,
    pub kind: LineKind,
    /// Set for [`LineKind::Frame`].
    pub frame: Option<Frame>,
}

impl Line {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: LineKind::Text,
            frame: None,
        }
    }
}

/// One thread of a dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub name: String,
    /// `RUNNABLE`, `WAITING` and so on; empty when the dump did not say.
    pub state: String,
    pub lines: Vec<Line>,
    /// The dump marked this thread as a part of a deadlock.
    pub deadlocked: bool,
    /// The dump said the thread waits for a lock.
    pub waiting_for_lock: bool,
}

impl Thread {
    /// Whether the thread has at least one resolved or unresolved frame.
    pub fn has_frames(&self) -> bool {
        self.lines.iter().any(|line| line.frame.is_some())
    }

    /// Whether the stack of the thread has a frame of the project.
    pub fn has_project_frames(&self) -> bool {
        self.lines.iter().any(|line| {
            line.frame
                .as_ref()
                .is_some_and(|frame| frame.origin == FrameOrigin::Project)
        })
    }
}

/// What the Analyze Stack Trace window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A stack trace: one list of lines.
    Trace { lines: Vec<Line> },
    /// A thread dump: the threads the program runs.
    Dump {
        threads: Vec<Thread>,
        deadlock: bool,
    },
}

impl Report {
    pub fn is_dump(&self) -> bool {
        matches!(self, Self::Dump { .. })
    }
}

/// The text a user pasted into Analyze Stack Trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceQuery(pub String);

/// The process whose threads the Run window asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DumpQuery(pub u32);

/// The answer of a request for the threads of a program: the report, or the reason
/// there is none, in words for the user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DumpAnswer {
    pub report: Option<Report>,
    pub error: Option<String>,
}

pub type TraceProvider = dyn Provider<TraceQuery, Report>;

pub type DumpProvider = dyn Provider<DumpQuery, DumpAnswer>;
