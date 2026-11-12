//! A refactoring that reaches several files: an open file changes through its buffer,
//! one undo step each, a closed one on disk. Moved files move after their edits.

use std::path::{Path, PathBuf};

use editor_view::{EditorView, uri_path};
use lang_api::{DocumentEdit, WorkspaceChange, apply_to_text};
use ui_kit::*;
use workspace::Workspace;

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn open_view(workspace: &Workspace, path: &Path) -> Option<Entity<EditorView>> {
    let tabs = workspace.open_tabs().into_iter().flatten();
    let open = tabs.into_iter().find(|tab| same_file(tab, path))?;
    workspace.view_of(&open)?.downcast::<EditorView>().ok()
}

struct PendingFile {
    path: PathBuf,
    text: String,
    format: text_core::FileFormat,
}

/// Applies the edits of closed files to their text on disk: every file keeps its
/// encoding, byte order mark and line ends, and either all of them change or none do.
/// Returns the files that could not be applied.
fn apply_to_files(files: &[(PathBuf, Vec<DocumentEdit>)]) -> Vec<String> {
    let mut pending: Vec<PendingFile> = Vec::with_capacity(files.len());
    let mut failed = Vec::new();
    for (path, edits) in files {
        if let Some(file) = pending.iter_mut().find(|file| file.path == *path) {
            file.text = apply_to_text(&file.text, edits);
            continue;
        }
        match text_core::read_file(path) {
            Ok(decoded) if !decoded.lossless => failed.push(format!(
                "{}: the file has bytes that are not text",
                path.display()
            )),
            Ok(decoded) => pending.push(PendingFile {
                path: path.clone(),
                text: apply_to_text(&decoded.text, edits),
                format: decoded.format,
            }),
            Err(error) => failed.push(format!("{}: {error}", path.display())),
        }
    }
    if !failed.is_empty() {
        return failed;
    }
    let updates: Vec<text_core::FileUpdate<'_>> = pending
        .iter()
        .map(|file| text_core::FileUpdate {
            path: &file.path,
            text: &file.text,
            format: &file.format,
        })
        .collect();
    match text_core::write_files(&updates) {
        Ok(()) => Vec::new(),
        Err(error) => vec![error.to_string()],
    }
}

/// Moves one file without ever writing over another one. A case-only rename names
/// the same file, so it is allowed; a different file at `to` is not.
fn rename_file(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() && !same_file(from, to) {
        return Err(format!(
            "{}: {} already exists",
            from.display(),
            to.display()
        ));
    }
    std::fs::rename(from, to).map_err(|error| format!("{}: {error}", from.display()))
}

/// What a workspace change asks for, once every open file is known and every rename
/// target is checked. Built before anything is written.
struct Plan {
    open: Vec<(PathBuf, Vec<Vec<DocumentEdit>>)>,
    closed: Vec<(PathBuf, Vec<DocumentEdit>)>,
    renames: Vec<(PathBuf, PathBuf)>,
}

impl Plan {
    fn new(change: WorkspaceChange, is_open: &dyn Fn(&Path) -> bool) -> Result<Self, Vec<String>> {
        let mut failed = Vec::new();
        let mut open: Vec<(PathBuf, Vec<Vec<DocumentEdit>>)> = Vec::new();
        let mut closed: Vec<(PathBuf, Vec<DocumentEdit>)> = Vec::new();
        for source in change.edits {
            if source.edits.is_empty() {
                continue;
            }
            let Some(path) = uri_path(&source.uri) else {
                failed.push(source.uri);
                continue;
            };
            if is_open(&path) {
                match open.iter_mut().find(|(known, _)| same_file(known, &path)) {
                    Some((_, batches)) => batches.push(source.edits),
                    None => open.push((path, vec![source.edits])),
                }
            } else {
                closed.push((path, source.edits));
            }
        }
        let mut renames = Vec::with_capacity(change.renames.len());
        for rename in change.renames {
            let (Some(from), Some(to)) = (uri_path(&rename.from), uri_path(&rename.to)) else {
                failed.push(rename.from);
                continue;
            };
            if to.exists() && !same_file(&from, &to) {
                failed.push(format!(
                    "{}: {} already exists",
                    from.display(),
                    to.display()
                ));
                continue;
            }
            renames.push((from, to));
        }
        if failed.is_empty() {
            Ok(Self {
                open,
                closed,
                renames,
            })
        } else {
            Err(failed)
        }
    }
}

/// What applying a plan does to open buffers. Kept apart from the window so that the
/// order of the writes can be tested.
trait Buffers {
    /// Takes the Local History snapshot that comes before a change on disk.
    fn snapshot(&mut self, path: &Path);
    /// Writes an open buffer whose file is about to move.
    fn save(&mut self, path: &Path) -> Result<(), String>;
    /// Replaces the text of an open buffer, one undo step per batch.
    fn apply(&mut self, path: &Path, batches: Vec<Vec<DocumentEdit>>);
    /// Moves the tabs of a file that has moved on disk.
    fn moved(&mut self, from: &Path, to: &Path);
}

/// Applies a change so that a failure leaves no half refactoring: every path and
/// rename target is checked first, then open buffers that move are saved, then closed
/// files all at once, then open buffers, then those buffers are saved again and the
/// files move. The first save proves the buffer can be written; the second one puts
/// the edits on disk, where the moved file takes them along.
fn apply_plan(
    change: WorkspaceChange,
    is_open: &dyn Fn(&Path) -> bool,
    buffers: &mut impl Buffers,
) -> Vec<String> {
    let plan = match Plan::new(change, is_open) {
        Ok(plan) => plan,
        Err(failed) => return failed,
    };
    let mut failed = Vec::new();
    for (from, _) in &plan.renames {
        buffers.snapshot(from);
        if is_open(from)
            && let Err(error) = buffers.save(from)
        {
            failed.push(error);
        }
    }
    if !failed.is_empty() {
        return failed;
    }
    for (path, _) in &plan.closed {
        buffers.snapshot(path);
    }
    let closed = apply_to_files(&plan.closed);
    if !closed.is_empty() {
        return closed;
    }
    for (path, batches) in plan.open {
        buffers.apply(&path, batches);
    }
    let mut moves = Vec::with_capacity(plan.renames.len());
    for (from, to) in plan.renames {
        if is_open(&from)
            && let Err(error) = buffers.save(&from)
        {
            failed.push(error);
        }
        moves.push((from, to));
    }
    if !failed.is_empty() {
        return failed;
    }
    for (from, to) in moves {
        if let Err(error) = rename_file(&from, &to) {
            failed.push(error);
            continue;
        }
        buffers.moved(&from, &to);
    }
    failed
}

struct LiveBuffers<'a, 'c> {
    workspace: &'a mut Workspace,
    window: &'a mut Window,
    cx: &'a mut Context<'c, Workspace>,
}

impl Buffers for LiveBuffers<'_, '_> {
    fn snapshot(&mut self, path: &Path) {
        crate::history::snapshot_file(path, local_history::Cause::BeforeReplace, self.cx);
    }

    fn save(&mut self, path: &Path) -> Result<(), String> {
        let Some(view) = open_view(self.workspace, path) else {
            return Ok(());
        };
        view.update(self.cx, |view, cx| view.save_blocking(cx))
            .map_or(Ok(()), |reason| {
                Err(format!("{}: {reason}", path.display()))
            })
    }

    fn apply(&mut self, path: &Path, batches: Vec<Vec<DocumentEdit>>) {
        let Some(view) = open_view(self.workspace, path) else {
            return;
        };
        let bytes = view.read(self.cx).file_bytes();
        crate::history::snapshot_bytes(path, bytes, local_history::Cause::BeforeReplace, self.cx);
        view.update(self.cx, |view, cx| {
            view.apply_document_batches(batches, cx);
        });
    }

    fn moved(&mut self, from: &Path, to: &Path) {
        self.workspace.paths_moved(from, to, self.window, self.cx);
    }
}

pub(crate) fn apply(
    workspace: &mut Workspace,
    change: WorkspaceChange,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let files = change
        .edits
        .iter()
        .filter(|source| !source.edits.is_empty() && uri_path(&source.uri).is_some())
        .count();
    let renames: Vec<(PathBuf, PathBuf)> = change
        .renames
        .iter()
        .filter_map(|rename| Some((uri_path(&rename.from)?, uri_path(&rename.to)?)))
        .collect();
    let open: Vec<PathBuf> = workspace.open_tabs().into_iter().flatten().collect();
    let is_open = |path: &Path| open.iter().any(|known| same_file(known, path));
    let mut buffers = LiveBuffers {
        workspace,
        window,
        cx,
    };
    let failed = apply_plan(change, &is_open, &mut buffers);
    if !failed.is_empty() {
        let text = format!("Not applied: {}", failed.join(", "));
        workspace.notify_user("Refactoring", text, cx);
        return;
    }
    if let Some(panel) = cx
        .try_global::<crate::project::ActiveProjectPanel>()
        .map(|panel| panel.0.clone())
    {
        // The window is being updated while the change is applied, so the rows follow
        // once it is free.
        cx.defer(move |cx| {
            for (from, to) in renames {
                panel.update(cx, |panel, cx| panel.paths_moved(&from, &to, cx));
            }
        });
    }
    if files > 1 {
        let text = format!("Changed {files} files");
        workspace.notify_user("Refactoring", text, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::{Buffers, apply_plan, apply_to_files, rename_file, same_file};
    use lang_api::{
        DocumentEdit, FileRename, SourceEdit, TextPoint, TextSpan, WorkspaceChange, apply_to_text,
    };

    fn replace(line: u32, start: u32, end: u32, text: &str) -> DocumentEdit {
        DocumentEdit {
            span: TextSpan {
                start: TextPoint {
                    line,
                    character: start,
                },
                end: TextPoint {
                    line,
                    character: end,
                },
            },
            text: text.to_owned(),
        }
    }

    fn directory(name: &str) -> std::path::PathBuf {
        test_support::scratch(&format!("changes-{name}"))
    }

    fn file_uri(path: &Path) -> String {
        format!("file:///{}", path.to_string_lossy().replace('\\', "/"))
    }

    fn source(path: &Path, text: &str) -> SourceEdit {
        let length = text.chars().count() as u32;
        SourceEdit {
            uri: file_uri(path),
            edits: vec![replace(0, 0, length, text)],
        }
    }

    #[derive(Default)]
    struct FakeBuffers {
        /// Text the open buffer holds, keyed by the path the buffer follows.
        text: HashMap<PathBuf, String>,
        /// Text that has reached the file on disk.
        disk: HashMap<PathBuf, String>,
        applied: Vec<PathBuf>,
        saved: Vec<PathBuf>,
        moved: Vec<(PathBuf, PathBuf)>,
        /// The one-based number of the save that fails, if any.
        fail_save: Option<usize>,
    }

    impl FakeBuffers {
        fn with(files: &[(&Path, &str)]) -> Self {
            let mut buffers = Self::default();
            for (path, text) in files {
                buffers.text.insert(path.to_path_buf(), (*text).to_owned());
                buffers.disk.insert(path.to_path_buf(), (*text).to_owned());
            }
            buffers
        }
    }

    impl Buffers for FakeBuffers {
        fn snapshot(&mut self, _path: &Path) {}

        fn save(&mut self, path: &Path) -> Result<(), String> {
            self.saved.push(path.to_path_buf());
            if self.fail_save == Some(self.saved.len()) {
                return Err(format!("{}: cannot write", path.display()));
            }
            if let Some(text) = self.text.get(path) {
                std::fs::write(path, text).unwrap();
                self.disk.insert(path.to_path_buf(), text.clone());
            }
            Ok(())
        }

        fn apply(&mut self, path: &Path, batches: Vec<Vec<DocumentEdit>>) {
            self.applied.push(path.to_path_buf());
            if let Some(text) = self.text.get_mut(path) {
                for edits in batches {
                    *text = apply_to_text(text, &edits);
                }
            }
        }

        fn moved(&mut self, from: &Path, to: &Path) {
            self.moved.push((from.to_path_buf(), to.to_path_buf()));
            if let Some(text) = self.text.remove(from) {
                self.text.insert(to.to_path_buf(), text);
            }
            if let Some(text) = self.disk.remove(from) {
                self.disk.insert(to.to_path_buf(), text);
            }
        }
    }

    #[test]
    fn a_closed_file_keeps_its_bom_and_crlf() {
        let directory = directory("bom-crlf");
        let path = directory.join("a.txt");
        std::fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\r\n").unwrap();

        let failed = apply_to_files(&[(path.clone(), vec![replace(0, 0, 3, "1")])]);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"\xEF\xBB\xBF1\r\ntwo\r\n");
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn one_read_only_file_leaves_every_file_untouched() {
        let directory = directory("read-only");
        let first = directory.join("a.txt");
        let second = directory.join("b.txt");
        std::fs::write(&first, "one").unwrap();
        std::fs::write(&second, "two").unwrap();
        let mut permissions = std::fs::metadata(&second).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&second, permissions).unwrap();

        let failed = apply_to_files(&[
            (first.clone(), vec![replace(0, 0, 3, "1")]),
            (second.clone(), vec![replace(0, 0, 3, "2")]),
        ]);
        assert!(
            failed.iter().any(|file| file.contains("b.txt")),
            "{failed:?}"
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");

        let mut permissions = std::fs::metadata(&second).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&second, permissions).unwrap();
    }

    #[test]
    fn a_rename_onto_an_existing_file_is_refused() {
        let directory = directory("rename");
        let from = directory.join("from.txt");
        let to = directory.join("to.txt");
        std::fs::write(&from, "from").unwrap();
        std::fs::write(&to, "to").unwrap();

        let error = rename_file(&from, &to).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(std::fs::read(&from).unwrap(), b"from");
        assert_eq!(std::fs::read(&to).unwrap(), b"to");
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn a_closed_file_failure_leaves_open_buffers_and_renames_alone() {
        let directory = directory("closed-failure");
        let open = directory.join("Open.txt");
        let closed = directory.join("Closed.txt");
        let from = directory.join("From.txt");
        let to = directory.join("To.txt");
        std::fs::write(&open, "open").unwrap();
        std::fs::write(&closed, "closed").unwrap();
        std::fs::write(&from, "from").unwrap();
        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&closed, permissions).unwrap();
        let change = WorkspaceChange {
            edits: vec![source(&open, "OPEN"), source(&closed, "CLOSED")],
            renames: vec![FileRename {
                from: file_uri(&from),
                to: file_uri(&to),
            }],
        };
        let mut buffers = FakeBuffers::with(&[(&open, "open")]);

        let failed = apply_plan(change, &|path| same_file(path, &open), &mut buffers);

        assert!(
            failed.iter().any(|file| file.contains("Closed.txt")),
            "{failed:?}"
        );
        assert_eq!(buffers.text[&open], "open");
        assert!(buffers.applied.is_empty(), "{:?}", buffers.applied);
        assert!(buffers.moved.is_empty(), "{:?}", buffers.moved);
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "from");
        assert!(!to.exists());

        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&closed, permissions).unwrap();
    }

    #[test]
    fn a_taken_rename_target_stops_the_whole_change() {
        let directory = directory("taken-target");
        let open = directory.join("Open.txt");
        let closed = directory.join("Closed.txt");
        let from = directory.join("From.txt");
        let to = directory.join("To.txt");
        std::fs::write(&open, "open").unwrap();
        std::fs::write(&closed, "closed").unwrap();
        std::fs::write(&from, "from").unwrap();
        std::fs::write(&to, "taken").unwrap();
        let change = WorkspaceChange {
            edits: vec![source(&open, "OPEN"), source(&closed, "CLOSED")],
            renames: vec![FileRename {
                from: file_uri(&from),
                to: file_uri(&to),
            }],
        };
        let mut buffers = FakeBuffers::with(&[(&open, "open")]);

        let failed = apply_plan(change, &|path| same_file(path, &open), &mut buffers);

        assert!(
            failed.iter().any(|file| file.contains("already exists")),
            "{failed:?}"
        );
        assert_eq!(buffers.text[&open], "open");
        assert!(buffers.applied.is_empty(), "{:?}", buffers.applied);
        assert!(buffers.saved.is_empty(), "{:?}", buffers.saved);
        assert!(buffers.moved.is_empty(), "{:?}", buffers.moved);
        assert_eq!(std::fs::read_to_string(&closed).unwrap(), "closed");
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "from");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "taken");
    }

    #[test]
    fn a_moved_open_file_takes_its_edit_along() {
        let directory = directory("moved-open");
        let from = directory.join("From.txt");
        let to = directory.join("To.txt");
        std::fs::write(&from, "old text").unwrap();
        let change = WorkspaceChange {
            edits: vec![source(&from, "new text")],
            renames: vec![FileRename {
                from: file_uri(&from),
                to: file_uri(&to),
            }],
        };
        let mut buffers = FakeBuffers::with(&[(&from, "old text")]);

        let failed = apply_plan(change, &|path| same_file(path, &from), &mut buffers);

        assert!(failed.is_empty(), "{failed:?}");
        assert!(!from.exists());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new text");
        assert_eq!(buffers.text.get(&to).map(String::as_str), Some("new text"));
        assert!(!buffers.text.contains_key(&from));
        assert_eq!(buffers.disk.get(&to).map(String::as_str), Some("new text"));
        assert_eq!(buffers.moved, vec![(from.clone(), to.clone())]);
    }

    #[test]
    fn a_failed_save_after_the_edits_keeps_them_in_the_buffer() {
        let directory = directory("second-save-fails");
        let from = directory.join("From.txt");
        let to = directory.join("To.txt");
        std::fs::write(&from, "old text").unwrap();
        let change = WorkspaceChange {
            edits: vec![source(&from, "new text")],
            renames: vec![FileRename {
                from: file_uri(&from),
                to: file_uri(&to),
            }],
        };
        let mut buffers = FakeBuffers::with(&[(&from, "old text")]);
        buffers.fail_save = Some(2);

        let failed = apply_plan(change, &|path| same_file(path, &from), &mut buffers);

        assert!(
            failed.iter().any(|error| error.contains("cannot write")),
            "{failed:?}"
        );
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "old text");
        assert!(!to.exists());
        assert_eq!(
            buffers.text.get(&from).map(String::as_str),
            Some("new text")
        );
        assert!(buffers.moved.is_empty(), "{:?}", buffers.moved);
        assert_eq!(buffers.saved, vec![from.clone(), from.clone()]);
    }
}
