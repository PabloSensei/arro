//! What a line of a console can open: a frame of a stack trace and a place a message of
//! a compiler names. The frame of the project opens its own file; the frame of a library
//! opens what the language has for it, when it has the source.

use std::path::{Path, PathBuf};

use editor_view::{Semantic, Spot};
use futures_util::StreamExt;
use lang_api::{Report, TraceQuery, ask};
use ui_kit::gpui::{App, WeakEntity, Window};
use workspace::Workspace;

use crate::navigation;

/// The frame of `at pkg.Cls.method(File.ext:42)`, without the module part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FrameSite {
    /// `pkg.Cls`, with the packages of the class.
    pub class: String,
    pub file: String,
    /// Counted from one.
    pub line: u32,
}

impl FrameSite {
    /// The file of the frame in the project: `pkg/Cls.ext` under any source root.
    pub fn project_file(&self, files: &[PathBuf]) -> Option<PathBuf> {
        let mut tail = PathBuf::new();
        let package = self
            .class
            .rsplit_once('.')
            .map_or("", |(package, _)| package);
        for part in package.split('.').filter(|part| !part.is_empty()) {
            tail.push(part);
        }
        tail.push(&self.file);
        files.iter().find(|path| path.ends_with(&tail)).cloned()
    }

    /// The extension of the file, for a source the language holds by a name of its own.
    pub fn extension(&self) -> String {
        extension_of(&self.file)
    }
}

/// Reads a frame such as `at pkg.Cls.method(File.ext:42)`, and the module form
/// `at module/pkg.Cls.method(File.ext:42)`.
pub(crate) fn frame_site(line: &str) -> Option<FrameSite> {
    let rest = line.trim_start().strip_prefix("at ")?;
    let open = rest.rfind('(')?;
    let close = rest[open..].rfind(')')? + open;
    let call = rest[..open].trim();
    let location = rest[open + 1..close].trim();
    if call.is_empty() || location.is_empty() {
        return None;
    }
    let (file, number) = location.rsplit_once(':')?;
    let line: u32 = number.trim().parse().ok().filter(|line| *line > 0)?;
    if file.is_empty() || file.contains(char::is_whitespace) {
        return None;
    }
    // The module of a named module comes before the slash.
    let call = call.rsplit('/').next().unwrap_or(call);
    let (class, method) = call.rsplit_once('.')?;
    if class.is_empty() || method.is_empty() {
        return None;
    }
    Some(FrameSite {
        class: class.to_owned(),
        file: file.to_owned(),
        line,
    })
}

/// The place of a message such as `src/Main.ext:42: cannot find it`: the file and the
/// line it names. A path with a space in it is not read, and neither is a URL.
pub(crate) fn compiler_site(line: &str) -> Option<(String, u32)> {
    let bytes = line.as_bytes();
    for (index, _) in line.char_indices() {
        if bytes[index] != b':' {
            continue;
        }
        let digits = index + 1;
        let mut end = digits;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == digits {
            continue;
        }
        if end < bytes.len() && bytes[end] != b':' && !bytes[end].is_ascii_whitespace() {
            continue;
        }
        let mut start = index;
        while start > 0 {
            let Some((previous, character)) = line[..start].char_indices().next_back() else {
                break;
            };
            if character.is_whitespace() || "\"'()[]{}<>,;".contains(character) {
                break;
            }
            start = previous;
        }
        let token = &line[start..index];
        if token.is_empty() || token.contains("://") {
            continue;
        }
        let Some((name, extension)) = token.rsplit_once('.') else {
            continue;
        };
        let named = extension.starts_with(|character: char| character.is_ascii_alphabetic())
            && extension
                .chars()
                .all(|character| character.is_ascii_alphanumeric());
        if name.is_empty() || !named {
            continue;
        }
        let number = line[digits..end].parse().ok()?;
        return Some((token.to_owned(), number));
    }
    None
}

/// Whether a line of the console is worth underlining when the pointer is over it.
pub(crate) fn looks_like_link(line: &str) -> bool {
    frame_site(line).is_some() || compiler_site(line).is_some()
}

/// The project file a compiler message names: the path itself, a path under the root, or
/// the tail of a path the message shortened.
pub(crate) fn compiler_file(candidate: &str, root: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(candidate);
    if path.is_absolute() {
        if path.is_file() {
            return Some(path.to_path_buf());
        }
    } else {
        let joined = root.join(path);
        if joined.is_file() {
            return Some(joined);
        }
    }
    let mut tail = PathBuf::new();
    for part in candidate
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && !part.ends_with(':'))
    {
        tail.push(part);
    }
    files
        .iter()
        .find(|file| file.ends_with(&tail))
        .cloned()
        .or_else(|| {
            let name = path.file_name()?;
            files
                .iter()
                .find(|file| file.file_name() == Some(name))
                .cloned()
        })
}

/// Opens the place of a frame the project does not hold through the language: a class of
/// a library opens its source when it is attached or can be fetched, and says so when the
/// language has none.
pub(crate) fn open_frame(
    text: &str,
    extension: String,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let support = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone());
    let Some(provider) = support.as_ref().and_then(|support| support.stack_traces()) else {
        missing(workspace, cx);
        return;
    };
    let mut replies = ask(provider, TraceQuery(text.to_owned()));
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let place = loop {
            let Some(answer) = replies.next().await else {
                break None;
            };
            let found = match answer.value {
                Report::Trace { lines } => lines
                    .into_iter()
                    .find_map(|line| line.frame.and_then(|frame| frame.place)),
                Report::Dump { threads, .. } => threads
                    .into_iter()
                    .flat_map(|thread| thread.lines)
                    .find_map(|line| line.frame.and_then(|frame| frame.place)),
            };
            if found.is_some() {
                break found;
            }
        };
        let Some(place) = place else {
            cx.update(|cx| missing(workspace, cx));
            return;
        };
        let line_at = place.span.start.line as usize;
        let open_workspace = workspace.clone();
        let missing_workspace = workspace.clone();
        cx.update(|cx| {
            editor_view::show_location(
                &place,
                &extension,
                cx,
                move |path, cx| {
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                let spot = Some(Spot::Line {
                                    line: line_at,
                                    column: 0,
                                });
                                open_workspace
                                    .update(cx, |workspace, cx| {
                                        navigation::go_to(workspace, path, spot, true, window, cx)
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                },
                move |cx| missing(missing_workspace, cx),
            );
        });
    })
    .detach();
}

/// Says under the window that the place of a frame has no source to open.
fn missing(workspace: WeakEntity<Workspace>, cx: &mut App) {
    workspace
        .update(cx, |workspace, cx| {
            workspace.notify_user("Console", "No source for this frame", cx)
        })
        .ok();
}

fn extension_of(file: &str) -> String {
    file.rsplit_once('.')
        .map_or_else(String::new, |(_, extension)| extension.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{compiler_file, compiler_site, frame_site, looks_like_link};

    #[test]
    fn a_frame_carries_its_class_file_and_line() {
        let site = frame_site("\tat demo.owner.OwnerTests$Nested.find(OwnerTests.ext:42)").unwrap();
        assert_eq!(site.class, "demo.owner.OwnerTests$Nested");
        assert_eq!(site.file, "OwnerTests.ext");
        assert_eq!(site.line, 42);

        let module = frame_site("at app//demo.Main.main(Main.ext:5)").unwrap();
        assert_eq!(module.class, "demo.Main");
        assert_eq!(module.line, 5);

        assert!(frame_site("at demo.Main.name(Unknown Source)").is_none());
        assert!(frame_site("Caused by: boom.Error: bad").is_none());
    }

    #[test]
    fn a_compiler_message_names_its_file_and_line() {
        let (file, line) = compiler_site("src/demo/Main.ext:12: cannot find the name").unwrap();
        assert_eq!(file, "src/demo/Main.ext");
        assert_eq!(line, 12);

        assert!(compiler_site("Process finished with exit code 1").is_none());
        assert!(compiler_site("at 12:30 the program started").is_none());
        assert!(compiler_site("see https://example.com/Page.ext:12").is_none());
        assert!(looks_like_link("src/demo/Main.ext:12: error"));
        assert!(!looks_like_link("hello from run"));
    }

    #[test]
    fn a_compiler_path_finds_the_file_of_the_project() {
        let file = PathBuf::from("C:/p/src/demo/Main.ext");
        let found = compiler_file(
            "src/demo/Main.ext",
            std::path::Path::new("C:/p"),
            std::slice::from_ref(&file),
        );
        assert_eq!(found, Some(file));
    }

    #[test]
    fn a_frame_finds_its_source_by_package() {
        let file = PathBuf::from("C:/p/src/main/demo/owner/OwnerTests.ext");
        let site = frame_site("at demo.owner.OwnerTests.find(OwnerTests.ext:42)").unwrap();
        assert_eq!(site.project_file(std::slice::from_ref(&file)), Some(file));
    }
}
