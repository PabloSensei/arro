//! A refactoring that reaches several files: an open file changes through its buffer,
//! a closed one on disk, and the window keeps the whole change as one step of its
//! history. Moved files move after their edits.

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

pub(crate) fn open_view(workspace: &Workspace, path: &Path) -> Option<Entity<EditorView>> {
    let tabs = workspace.open_tabs().into_iter().flatten();
    let open = tabs.into_iter().find(|tab| same_file(tab, path))?;
    workspace.view_of(&open)?.downcast::<EditorView>().ok()
}

struct PendingFile {
    path: PathBuf,
    text: String,
    format: text_core::FileFormat,
    /// The text the file held before the change; `None` for a file it brings into being.
    before: Option<String>,
    before_format: text_core::FileFormat,
    /// The change brings the file into being, so there is no text to keep around it.
    new: bool,
}

/// A closed file a change wrote, with what it held before: the window keeps this to
/// take the whole change back in one step.
#[derive(Clone)]
pub(crate) struct WrittenFile {
    pub path: PathBuf,
    pub before: Option<String>,
    pub before_format: text_core::FileFormat,
    pub after: String,
    pub after_format: text_core::FileFormat,
}

/// Applies the edits of closed files to their text on disk: every file keeps its
/// encoding, byte order mark and line ends, and either all of them change or none do.
/// A file that does not exist yet is created from the text of its edits; a failure
/// takes the new files away again. Returns the files that could not be applied.
#[cfg(test)]
fn apply_to_files(files: &[(PathBuf, Vec<DocumentEdit>)]) -> Vec<String> {
    apply_to_files_with(files).0
}

/// The same, with the text every file held before and after, for the undo of the whole
/// change.
fn apply_to_files_with(files: &[(PathBuf, Vec<DocumentEdit>)]) -> (Vec<String>, Vec<WrittenFile>) {
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
                format: decoded.format.clone(),
                before: Some(decoded.text),
                before_format: decoded.format,
                new: false,
            }),
            // An extract interface and the like bring a file into being: its edits are
            // the whole text of the new file.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                pending.push(PendingFile {
                    path: path.clone(),
                    text: apply_to_text("", edits),
                    format: text_core::FileFormat::default(),
                    before: None,
                    before_format: text_core::FileFormat::default(),
                    new: true,
                });
            }
            Err(error) => failed.push(format!("{}: {error}", path.display())),
        }
    }
    if !failed.is_empty() {
        return (failed, Vec::new());
    }
    // The new files come first: nothing of the old ones has changed if one fails, and
    // a failure of the old ones takes the new files away again.
    let mut created = Vec::new();
    for file in pending.iter().filter(|file| file.new) {
        let written = file
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| text_core::write_file(&file.path, &file.text, &file.format));
        match written {
            Ok(()) => created.push(file.path.clone()),
            Err(error) => {
                remove_files(&created);
                return (
                    vec![format!("{}: {error}", file.path.display())],
                    Vec::new(),
                );
            }
        }
    }
    let updates: Vec<text_core::FileUpdate<'_>> = pending
        .iter()
        .filter(|file| !file.new)
        .map(|file| text_core::FileUpdate {
            path: &file.path,
            text: &file.text,
            format: &file.format,
        })
        .collect();
    let result = match text_core::write_files(&updates) {
        Ok(()) => Vec::new(),
        Err(error) => {
            remove_files(&created);
            vec![error.to_string()]
        }
    };
    if !result.is_empty() {
        return (result, Vec::new());
    }
    let written = pending
        .into_iter()
        .map(|file| WrittenFile {
            path: file.path,
            before: file.before,
            before_format: file.before_format,
            after: file.text,
            after_format: file.format,
        })
        .collect();
    (Vec::new(), written)
}

fn remove_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
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
    /// A closed file reached the disk, with what it held before.
    fn written(&mut self, _file: &WrittenFile) {}
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
    let (closed, written) = apply_to_files_with(&plan.closed);
    if !closed.is_empty() {
        return closed;
    }
    for file in &written {
        buffers.written(file);
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
    /// The reason the snapshots name: a refactoring, a reformat or another change.
    cause: local_history::Cause,
    /// The open files the change applied to, in the order it named them.
    open: Vec<PathBuf>,
    /// The closed files that reached the disk.
    written: Vec<WrittenFile>,
    /// The files that moved.
    renames: Vec<(PathBuf, PathBuf)>,
}

impl Buffers for LiveBuffers<'_, '_> {
    fn snapshot(&mut self, path: &Path) {
        match open_view(self.workspace, path) {
            Some(view) => {
                let bytes = view.read(self.cx).file_bytes();
                crate::history::snapshot_bytes(path, bytes, self.cause, self.cx);
            }
            None => crate::history::snapshot_now(path, self.cause, self.cx),
        }
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
        self.open.push(path.to_path_buf());
        let Some(view) = open_view(self.workspace, path) else {
            return;
        };
        let bytes = view.read(self.cx).file_bytes();
        crate::history::snapshot_bytes(path, bytes, self.cause, self.cx);
        view.update(self.cx, |view, cx| {
            view.apply_document_batches(batches, cx);
        });
    }

    fn moved(&mut self, from: &Path, to: &Path) {
        self.renames.push((from.to_path_buf(), to.to_path_buf()));
        self.workspace.paths_moved(from, to, self.window, self.cx);
    }

    fn written(&mut self, file: &WrittenFile) {
        self.written.push(file.clone());
    }
}

pub(crate) fn apply(
    workspace: &mut Workspace,
    change: WorkspaceChange,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    apply_cause(
        workspace,
        change,
        None,
        None,
        local_history::Cause::BeforeRefactoring,
        window,
        cx,
    );
}

/// Reformat Code changes a file the way a refactoring does; its snapshots say Reformat.
pub(crate) fn apply_reformat(
    workspace: &mut Workspace,
    change: WorkspaceChange,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    apply_cause(
        workspace,
        change,
        None,
        None,
        local_history::Cause::BeforeReformat,
        window,
        cx,
    );
}

/// Applies a change that a refactoring started in `origin`, whose name `label` gives to
/// Local History. A change of several files becomes one entry of the window's history:
/// its undo takes every file back, closed ones on disk and created ones away.
pub(crate) fn apply_from(
    workspace: &mut Workspace,
    change: WorkspaceChange,
    origin: Option<Entity<EditorView>>,
    label: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    apply_cause(
        workspace,
        change,
        origin,
        label,
        local_history::Cause::BeforeRefactoring,
        window,
        cx,
    );
}

fn apply_cause(
    workspace: &mut Workspace,
    change: WorkspaceChange,
    origin: Option<Entity<EditorView>>,
    label: Option<String>,
    cause: local_history::Cause,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let files = change
        .edits
        .iter()
        .filter(|source| !source.edits.is_empty() && uri_path(&source.uri).is_some())
        .count();
    let open: Vec<PathBuf> = workspace.open_tabs().into_iter().flatten().collect();
    let is_open = |path: &Path| open.iter().any(|known| same_file(known, path));
    let origin_path = origin
        .as_ref()
        .map(|view| view.read(cx).path().to_path_buf());
    let (failed, applied, written, moved) = {
        let mut buffers = LiveBuffers {
            workspace,
            window,
            cx,
            cause,
            open: Vec::new(),
            written: Vec::new(),
            renames: Vec::new(),
        };
        let failed = apply_plan(change, &is_open, &mut buffers);
        (failed, buffers.open, buffers.written, buffers.renames)
    };
    if !failed.is_empty() {
        let text = format!("Not applied: {}", failed.join(", "));
        workspace.notify_user("Refactoring", text, cx);
        return;
    }
    if let Some(label) = &label {
        crate::history::put_label(workspace.root(), label.clone(), cx);
    }
    if let Some(panel) = cx
        .try_global::<crate::project::ActiveProjectPanel>()
        .map(|panel| panel.0.clone())
    {
        // The window is being updated while the change is applied, so the rows follow
        // once it is free.
        let rows = moved.clone();
        cx.defer(move |cx| {
            for (from, to) in rows {
                panel.update(cx, |panel, cx| panel.paths_moved(&from, &to, cx));
            }
        });
    }
    // A moved file takes its tab along, which the undo of the text alone cannot put
    // back: such a change stays with the undo of each buffer.
    let touched = applied.len() + written.len() + moved.len();
    if touched > 1 && moved.is_empty() {
        let group = crate::refactor_undo::Group {
            origin: origin_path.or_else(|| applied.first().cloned()),
            open: applied,
            closed: written,
        };
        let origin = origin.or_else(|| {
            group
                .origin
                .as_deref()
                .and_then(|path| open_view(workspace, path))
        });
        crate::refactor_undo::push(workspace, group, origin, cx);
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
    fn a_change_brings_a_new_file_into_being_from_an_empty_text() {
        let directory = directory("new-file");
        let made = directory.join("new").join("Made.txt");

        let failed = apply_to_files(&[(made.clone(), vec![replace(0, 0, 0, "made\n")])]);

        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(std::fs::read_to_string(&made).unwrap(), "made\n");
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn a_failed_file_leaves_no_new_file_behind() {
        let directory = directory("new-file-rollback");
        let made = directory.join("Made.txt");
        let closed = directory.join("Closed.txt");
        std::fs::write(&closed, "closed").unwrap();
        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&closed, permissions).unwrap();

        let failed = apply_to_files(&[
            (made.clone(), vec![replace(0, 0, 0, "made\n")]),
            (closed.clone(), vec![replace(0, 0, 6, "CLOSED")]),
        ]);

        assert!(
            failed.iter().any(|file| file.contains("Closed.txt")),
            "{failed:?}"
        );
        assert!(!made.exists());

        let mut permissions = std::fs::metadata(&closed).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&closed, permissions).unwrap();
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
