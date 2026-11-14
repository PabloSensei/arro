//! The boundary with the editor: `workspace` shows tabs, it does not know what
//! is inside them. The application decides which view opens a file.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use ui_kit::*;

/// Line and column of a caret, from zero.
pub type Caret = (usize, usize);

/// Puts the caret somewhere.
pub type CaretSetter = Rc<dyn Fn(Caret, &mut App)>;

/// Changes how a file is written, by a name from [`LINE_ENDS`] or [`ENCODINGS`].
pub type FormatSetter = Rc<dyn Fn(&str, &mut App)>;

/// The indentation the user picks for one open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentChoice {
    /// Indent with tabs.
    Tabs,
    /// Indent with spaces of the size the file has now.
    Spaces,
    /// A new size of one step for this file.
    Size(usize),
}

/// Indents one open file until it closes.
pub type IndentSetter = Rc<dyn Fn(IndentChoice, &mut App)>;

/// Writes unsaved changes before it returns. `Some` says why the text is still not on disk.
pub type SaveNow = Rc<dyn Fn(&mut App) -> Option<String>>;

/// Shows the rendered document of a tab alone, as a README opens.
pub type PreviewOnly = Rc<dyn Fn(&mut App)>;

/// What the status bar shows about the active tab.
#[derive(Clone, Default)]
pub struct EditorStatus {
    pub caret: String,
    pub eol: String,
    pub encoding: String,
    pub indent: String,
    /// The language of the file, as the application names it, for the indent menu.
    pub language: Option<String>,
    /// The file indents with tabs, for the menu of the status bar.
    pub indent_tabs: bool,
    /// The size of one indentation step.
    pub indent_size: usize,
    /// A `.editorconfig` file named the indentation.
    pub indent_editorconfig: bool,
    pub dirty: bool,
    pub insert: String,
    /// The file has the read-only attribute on disk.
    pub read_only: bool,
}

/// An open editor as the workspace sees it.
#[derive(Clone)]
pub struct EditorTab {
    pub view: AnyView,
    pub focus: FocusHandle,
    pub status: Rc<dyn Fn(&App) -> EditorStatus>,
    /// Writes unsaved changes off the UI thread, for a closing tab.
    pub save: Rc<dyn Fn(&mut App)>,
    /// Writes unsaved changes before it returns, for a closing window.
    pub save_now: SaveNow,
    /// Line and column of the caret from zero, for the layout saved with the project.
    pub caret: Rc<dyn Fn(&App) -> Caret>,
    /// Puts the caret there; the file may still be loading.
    pub restore_caret: CaretSetter,
    /// Changes how the file is written: a line end (`LF`, `CRLF`, `CR`) or an encoding name.
    pub set_format: FormatSetter,
    /// Changes the indentation of the file until its tab closes.
    pub set_indent: IndentSetter,
    /// Takes the text on disk in place of the edits of the buffer; undo brings them back.
    pub reload_from_disk: Rc<dyn Fn(&mut App)>,
    /// Shows the rendered document of the tab alone, the way a README opens; `None` for
    /// a file without a preview.
    pub show_preview_only: Option<PreviewOnly>,
}

/// Line ends and encodings the status bar offers, by the names `EditorTab::set_format` takes.
pub const LINE_ENDS: [&str; 3] = ["LF", "CRLF", "CR"];
pub const ENCODINGS: [&str; 4] = ["UTF-8", "UTF-8 BOM", "UTF-16LE", "windows-1251"];

/// Opens a file. Must not read it on the calling thread.
pub type EditorFactory = Rc<dyn Fn(PathBuf, &mut Window, &mut App) -> EditorTab>;

/// Icons by file name or extension, given by the application: neutral crates
/// know no languages.
#[derive(Clone, Default)]
pub struct FileIcons {
    pub by_name: Vec<(&'static str, &'static str)>,
    pub fallback: &'static str,
}

impl FileIcons {
    pub fn icon_for(&self, path: &Path) -> &'static str {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let extension = path.extension().unwrap_or_default().to_string_lossy();
        let key = |key: &&(&str, &str)| key.0 == name || key.0 == extension;
        self.by_name
            .iter()
            .find(key)
            .map_or(self.fallback, |entry| entry.1)
    }
}

#[cfg(test)]
mod tests {
    use super::FileIcons;
    use std::path::Path;

    #[test]
    fn looks_up_by_name_then_extension() {
        let icons = FileIcons {
            by_name: vec![(".gitignore", "git"), ("xml", "xml")],
            fallback: "text",
        };
        assert_eq!(icons.icon_for(Path::new("a/.gitignore")), "git");
        assert_eq!(icons.icon_for(Path::new("a/pom.xml")), "xml");
        assert_eq!(icons.icon_for(Path::new("a/readme")), "text");
    }
}
