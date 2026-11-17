//! The themes of a window: the built-in ones the binary carries and the files of the
//! `themes` folder of the settings. A broken file is left out and named, so that one
//! theme of a user cannot stop the program.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::format::{ThemeError, ThemeFile};
use crate::{Color, Tokens, builtin};

/// The folder of the settings that holds the themes of the user.
pub fn themes_folder(settings_dir: &Path) -> PathBuf {
    settings_dir.join("themes")
}

/// A theme the window can use, already resolved against its parent.
#[derive(Clone, Debug)]
pub struct ThemeDefinition {
    /// The name the settings save, and the theme is looked up by.
    pub id: String,
    /// The label the settings show.
    pub name: String,
    pub dark: bool,
    pub tokens: Tokens<Color>,
    /// A theme of the binary, which the settings do not offer to delete.
    pub builtin: bool,
    /// The file of a theme of the user.
    pub path: Option<PathBuf>,
}

/// The built-in themes and the themes of the settings folder.
#[derive(Clone, Debug)]
pub struct ThemeLibrary {
    themes: Vec<ThemeDefinition>,
    errors: Vec<ThemeError>,
}

impl ThemeLibrary {
    /// The themes the binary carries, without the file system.
    pub fn builtin() -> &'static Self {
        static BUILTIN: OnceLock<ThemeLibrary> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            let (themes, errors) = builtin::resolve_all();
            Self { themes, errors }
        })
    }

    /// The built-in themes and the files of `folder`, built-ins first in their own order,
    /// then the files by name. A missing folder has no themes.
    pub fn load(folder: &Path) -> Self {
        let mut library = Self::builtin().clone();
        let Ok(entries) = std::fs::read_dir(folder) else {
            return library;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            })
            .collect();
        files.sort();
        for path in files {
            library.add_file(&path);
        }
        library
    }

    /// Reads one file into the library, or records why it cannot be used.
    fn add_file(&mut self, path: &Path) {
        let Some(id) = theme_id(path) else {
            self.errors
                .push(ThemeError::at(path, "the file name gives no theme name"));
            return;
        };
        if self.get(&id).is_some() {
            self.errors.push(ThemeError::at(
                path,
                format!("a theme named {id} is already there, the file is skipped"),
            ));
            return;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.errors
                    .push(ThemeError::at(path, format!("not read: {error}")));
                return;
            }
        };
        let file = match ThemeFile::parse(&text) {
            Ok(file) => file,
            Err(error) => {
                self.errors.push(ThemeError::at(path, error.message));
                return;
            }
        };
        let parent = match file.parent.as_deref() {
            Some(parent) => match self.get(parent) {
                Some(theme) => Some(theme.tokens),
                None => {
                    self.errors.push(ThemeError::at(
                        path,
                        format!("the parent theme {parent} is not there"),
                    ));
                    return;
                }
            },
            None => None,
        };
        let (tokens, warnings) = match file.resolve(parent) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.errors.push(ThemeError::at(path, error.message));
                return;
            }
        };
        for warning in warnings {
            self.errors.push(ThemeError::at(path, warning));
        }
        self.themes.push(ThemeDefinition {
            id,
            name: file.name,
            dark: file.dark,
            tokens,
            builtin: false,
            path: Some(path.to_path_buf()),
        });
    }

    pub fn themes(&self) -> &[ThemeDefinition] {
        &self.themes
    }

    pub fn get(&self, id: &str) -> Option<&ThemeDefinition> {
        self.themes.iter().find(|theme| theme.id == id)
    }

    /// The themes left out or warned about, in the order they were read.
    pub fn errors(&self) -> &[ThemeError] {
        &self.errors
    }

    /// The id of the first theme of the look `dark` asks for, or of the first theme at
    /// all, for a window that has to open with something.
    pub fn fallback(&self, dark: bool) -> &str {
        self.themes
            .iter()
            .find(|theme| theme.dark == dark)
            .or_else(|| self.themes.first())
            .map_or("dark", |theme| theme.id.as_str())
    }
}

/// The id of a theme file: its name without `.theme.json` or `.json`.
fn theme_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let name = name
        .strip_suffix(".theme.json")
        .or_else(|| name.strip_suffix(".json"))?;
    (!name.is_empty()).then(|| name.to_owned())
}

impl Default for ThemeLibrary {
    fn default() -> Self {
        Self::builtin().clone()
    }
}

/// A name that is safe as the id of an imported theme: letters and digits of the ASCII
/// range stay, a run of everything else becomes one `-`.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(character.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("imported");
    }
    slug
}

/// The files of the built-in themes, resolved once.
pub(crate) fn resolve_builtin(
    files: &[(&'static str, &'static str)],
) -> (Vec<ThemeDefinition>, Vec<ThemeError>) {
    let mut parsed: HashMap<&str, ThemeFile> = HashMap::new();
    let mut errors = Vec::new();
    for (id, text) in files {
        match ThemeFile::parse(text) {
            Ok(file) => {
                parsed.insert(id, file);
            }
            Err(error) => errors.push(ThemeError::new(format!(
                "the built-in theme {id} is not read: {}",
                error.message
            ))),
        }
    }
    let mut themes: Vec<ThemeDefinition> = Vec::new();
    for (id, _) in files {
        let Some(file) = parsed.get(id) else {
            continue;
        };
        let parent = match file.parent.as_deref() {
            Some(parent) => match parsed.get(parent) {
                Some(parent_file) => match parent_file.resolve(theme_parent(&parsed, parent)) {
                    Ok((tokens, _)) => Some(tokens),
                    Err(error) => {
                        errors.push(ThemeError::new(format!(
                            "the built-in theme {id} has a broken parent: {}",
                            error.message
                        )));
                        continue;
                    }
                },
                None => {
                    errors.push(ThemeError::new(format!(
                        "the built-in theme {id} names the missing parent {parent}"
                    )));
                    continue;
                }
            },
            None => None,
        };
        match file.resolve(parent) {
            Ok((tokens, warnings)) => {
                for warning in warnings {
                    errors.push(ThemeError::new(format!(
                        "the built-in theme {id}: {warning}"
                    )));
                }
                themes.push(ThemeDefinition {
                    id: (*id).to_owned(),
                    name: file.name.clone(),
                    dark: file.dark,
                    tokens,
                    builtin: true,
                    path: None,
                });
            }
            Err(error) => errors.push(ThemeError::new(format!(
                "the built-in theme {id} is not used: {}",
                error.message
            ))),
        }
    }
    (themes, errors)
}

/// The tokens of a parent of a built-in theme, which itself may have a parent.
fn theme_parent(parsed: &HashMap<&str, ThemeFile>, id: &str) -> Option<Tokens<Color>> {
    let file = parsed.get(id)?;
    let parent = file
        .parent
        .as_deref()
        .and_then(|parent| theme_parent(parsed, parent));
    file.resolve(parent).ok().map(|(tokens, _)| tokens)
}
