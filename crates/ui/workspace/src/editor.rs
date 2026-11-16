//! The boundary with the editor: `workspace` shows tabs, it does not know what
//! is inside them. The application decides which view opens a file.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
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

/// What a tab with a rendered document shows: the text, the document, or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewLayout {
    EditorOnly,
    #[default]
    Both,
    PreviewOnly,
}

/// What a tab of a file of a localized set shows: its text or the table of the set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleLayout {
    #[default]
    Text,
    Bundle,
}

/// Reads what a tab of a file of a localized set shows.
pub type BundleReader = Rc<dyn Fn(&App) -> BundleLayout>;

/// Changes what a tab of a file of a localized set shows.
pub type BundleSetter = Rc<dyn Fn(BundleLayout, &mut App)>;

/// Reads and changes what a tab of a file of a localized set shows: the tab itself and
/// the mode the layout of the project keeps for the file.
#[derive(Clone)]
pub struct BundleTab {
    pub layout: BundleReader,
    pub set_layout: BundleSetter,
}

/// Reads what a tab with a rendered document shows.
pub type PreviewReader = Rc<dyn Fn(&App) -> PreviewLayout>;

/// Changes what a tab with a rendered document shows.
pub type PreviewSetter = Rc<dyn Fn(PreviewLayout, &mut App)>;
/// Asks whether a tab may close; `false` keeps it open while its view asks the user.
pub type CloseGuard = Rc<dyn Fn(&mut App) -> bool>;

/// Reads and changes what a tab with a rendered document shows: the tab itself and the
/// mode the layout of the project keeps for the file.
#[derive(Clone)]
pub struct PreviewTab {
    pub layout: PreviewReader,
    pub set_layout: PreviewSetter,
}

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
    /// What the tab with a rendered document shows; `None` for a file without a preview.
    pub preview: Option<PreviewTab>,
    /// What the tab of a file of a localized set shows; `None` for other files.
    pub bundle: Option<BundleTab>,
    /// Asks whether the tab may close. `false` keeps it open: a view with unsaved work
    /// shows its question here and closes itself once the user has answered. `None`
    /// closes the tab at once.
    pub can_close: Option<CloseGuard>,
    /// A tab that leaves the screen, such as the one another tab replaces: its view drops
    /// what it showed over the text. `None` for a tab without such a view.
    pub on_hidden: Option<HiddenGuard>,
}

/// What a tab does when it leaves the screen.
pub type HiddenGuard = Rc<dyn Fn(&mut App)>;

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
        let key = |key: &&(&str, &str)| {
            key.0.eq_ignore_ascii_case(&name) || key.0.eq_ignore_ascii_case(&extension)
        };
        self.by_name
            .iter()
            .find(key)
            .map_or(self.fallback, |entry| entry.1)
    }

    /// The icons of the languages the editor knows, by extension and by file name.
    /// The application adds its own files first, so a language it names itself wins.
    pub fn with_languages(mut self) -> Self {
        self.by_name.extend(LANGUAGE_ICONS);
        self
    }
}

/// The icons of the languages the editor knows, by extension and by file name. The
/// neutral crate only names file types; a language it knows through another crate stays
/// the business of the application.
const LANGUAGE_ICONS: &[(&str, &str)] = &[
    ("Dockerfile", "fileTypes/dockerfile"),
    ("Gemfile", "fileTypes/ruby"),
    ("Rakefile", "fileTypes/ruby"),
    (".bashrc", "fileTypes/shell"),
    (".zshrc", "fileTypes/shell"),
    (".profile", "fileTypes/shell"),
    ("py", "fileTypes/python"),
    ("pyw", "fileTypes/python"),
    ("c", "fileTypes/c"),
    ("h", "fileTypes/c"),
    ("cpp", "fileTypes/cpp"),
    ("cc", "fileTypes/cpp"),
    ("cxx", "fileTypes/cpp"),
    ("hpp", "fileTypes/cpp"),
    ("hh", "fileTypes/cpp"),
    ("hxx", "fileTypes/cpp"),
    ("cs", "fileTypes/csharp"),
    ("go", "fileTypes/go"),
    ("rs", "fileTypes/rust"),
    ("scala", "fileTypes/scala"),
    ("sc", "fileTypes/scala"),
    ("php", "fileTypes/php"),
    ("rb", "fileTypes/ruby"),
    ("gemspec", "fileTypes/ruby"),
    ("swift", "fileTypes/swift"),
    ("lua", "fileTypes/lua"),
    ("ps1", "fileTypes/powershell"),
    ("psm1", "fileTypes/powershell"),
    ("psd1", "fileTypes/powershell"),
    ("proto", "fileTypes/protobuf"),
    ("kt", "fileTypes/kotlin"),
    ("kts", "fileTypes/kotlin"),
    ("groovy", "fileTypes/groovy"),
    ("gvy", "fileTypes/groovy"),
    ("js", "fileTypes/javascript"),
    ("jsx", "fileTypes/javascript"),
    ("mjs", "fileTypes/javascript"),
    ("cjs", "fileTypes/javascript"),
    ("ts", "fileTypes/typescript"),
    ("tsx", "fileTypes/typescript"),
    ("mts", "fileTypes/typescript"),
    ("cts", "fileTypes/typescript"),
    ("json", "fileTypes/json"),
    ("yaml", "fileTypes/yaml"),
    ("yml", "fileTypes/yaml"),
    ("toml", "fileTypes/toml"),
    ("sql", "fileTypes/sql"),
    ("html", "fileTypes/html"),
    ("htm", "fileTypes/html"),
    ("css", "fileTypes/css"),
    ("md", "fileTypes/markdown"),
    ("markdown", "fileTypes/markdown"),
    ("sh", "fileTypes/shell"),
    ("bash", "fileTypes/shell"),
    ("zsh", "fileTypes/shell"),
    ("fish", "fileTypes/shell"),
    ("dockerfile", "fileTypes/dockerfile"),
];

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
