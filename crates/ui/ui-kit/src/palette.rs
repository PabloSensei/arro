//! The theme in colours of GPUI, and the catalog of the themes a window offers.

use std::collections::HashSet;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::*;
use theme::{Color, Tokens};
pub use theme::{
    ThemeDefinition, ThemeError, ThemeFile, ThemeLibrary, from_icls, slug, themes_folder,
};

fn hsla_of(color: Color) -> Hsla {
    let Color { r, g, b, a } = color;
    Rgba { r, g, b, a }.into()
}

fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b) = (from.to_rgb(), to.to_rgb());
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

/// Keeps the id of a theme of the user for the life of the process, so that [`Theme`]
/// stays small and copyable. A theme folder holds a handful of ids.
fn intern(id: &str) -> &'static str {
    static IDS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let ids = IDS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut ids = ids.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(known) = ids.get(id) {
        return known;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    ids.insert(leaked);
    leaked
}

/// A theme in colours of GPUI.
#[derive(Clone, Copy)]
pub struct Theme {
    tokens: Tokens<Hsla>,
    id: &'static str,
}

impl Deref for Theme {
    type Target = Tokens<Hsla>;

    fn deref(&self) -> &Self::Target {
        &self.tokens
    }
}

impl Global for Theme {}

impl Theme {
    /// The two names the settings used before themes became files. The settings keep
    /// cycling them; the list of all themes lives in [`ThemeCatalog`].
    pub const NAMES: [&'static str; 2] = ["dark", "light"];

    pub fn dark() -> Self {
        Self::named("dark")
    }

    pub fn light() -> Self {
        Self::named("light")
    }

    /// A built-in theme by the name a theme file gives it. A name of a later release, or
    /// a damaged one, falls back to the built-in dark theme; a theme of the settings
    /// folder comes from [`ThemeCatalog`].
    pub fn named(name: &str) -> Self {
        let library = ThemeLibrary::builtin();
        library
            .get(name)
            .map_or_else(Self::dark, Self::from_definition)
    }

    /// A resolved theme of the catalog.
    pub fn from_definition(definition: &ThemeDefinition) -> Self {
        Self {
            tokens: definition.tokens.map(hsla_of),
            id: intern(&definition.id),
        }
    }

    /// The theme of the catalog called `id`, or the dark one when it is not there.
    pub fn from_library(library: &ThemeLibrary, id: &str) -> Self {
        library.get(id).map_or_else(
            || Self::named(library.fallback(true)),
            Self::from_definition,
        )
    }

    /// The id the theme is saved by.
    pub fn id(self) -> &'static str {
        self.id
    }

    /// The label of a saved id, as the Settings window writes it. A theme of the settings
    /// folder shows the id; the list of themes has its display name.
    pub fn label(name: &str) -> String {
        ThemeLibrary::builtin()
            .get(name)
            .map_or_else(|| name.to_owned(), |theme| theme.name.clone())
    }

    /// A built-in theme with some tokens replaced, colours as 0xRRGGBBAA. Returns the
    /// names that are no token.
    pub fn with_colors<'a>(
        dark: bool,
        colors: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> (Self, Vec<String>) {
        let mut tokens = if dark {
            Tokens::dark()
        } else {
            Tokens::light()
        };
        let mut unknown = Vec::new();
        for (name, hex) in colors {
            if !tokens.set(name, Color::from_hex(hex)) {
                unknown.push(name.to_owned());
            }
        }
        let id = intern(if dark { "dark" } else { "light" });
        (
            Self {
                tokens: tokens.map(hsla_of),
                id,
            },
            unknown,
        )
    }

    /// Cross-fade used by the theme switch animation.
    pub fn mix(from: &Self, to: &Self, t: f32) -> Self {
        let mut tokens = from.tokens.zip(&to.tokens, |a, b| mix(a, b, t));
        tokens.dark = if t < 0.5 { from.dark } else { to.dark };
        Self {
            tokens,
            id: if t < 0.5 { from.id } else { to.id },
        }
    }
}

/// The themes a window offers: the built-in ones and the files of its settings folder.
#[derive(Clone)]
pub struct ThemeCatalog {
    library: Arc<ThemeLibrary>,
    folder: PathBuf,
}

impl Global for ThemeCatalog {}

impl ThemeCatalog {
    /// Reads the `themes` folder of a settings folder. A broken file is left out with a
    /// line in the journal; the Settings window shows the same line.
    pub fn load(settings_dir: &Path) -> Self {
        let folder = theme::themes_folder(settings_dir);
        let library = ThemeLibrary::load(&folder);
        for error in library.errors() {
            eprintln!("theme: {error}");
        }
        Self {
            library: Arc::new(library),
            folder,
        }
    }

    /// The themes of the binary alone.
    pub fn builtin() -> Self {
        Self {
            library: Arc::new(ThemeLibrary::builtin().clone()),
            folder: PathBuf::new(),
        }
    }

    pub fn library(&self) -> &ThemeLibrary {
        &self.library
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Reads the folder again after an import or a file of the user changed.
    pub fn reload(&mut self) {
        if self.folder.as_os_str().is_empty() {
            return;
        }
        let library = ThemeLibrary::load(&self.folder);
        for error in library.errors() {
            eprintln!("theme: {error}");
        }
        self.library = Arc::new(library);
    }

    /// The theme called `id`, or the dark one when the id is not there.
    pub fn theme(&self, id: &str) -> Theme {
        Theme::from_library(&self.library, id)
    }

    /// The themes of the window, or the built-in ones when a window has not loaded its
    /// folder.
    pub fn library_of(cx: &App) -> &ThemeLibrary {
        match cx.try_global::<ThemeCatalog>() {
            Some(catalog) => catalog.library(),
            None => ThemeLibrary::builtin(),
        }
    }
}
