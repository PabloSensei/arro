//! The spelling check of the open editors: the words a language names, the dictionaries
//! of the user and of the system, and the marks the text shows. The check runs off the
//! UI thread and its answer is dropped when the text has moved on.

use std::path::PathBuf;
use std::sync::Arc;

use editor_view::{EditorView, InspectionProfile, Semantic, SpellingMark};
use lang_api::{InspectionLevel, SpellQuery};
use spelling::{Region, Speller};
use ui_kit::gpui::{App, Context, Entity};
use workspace::Workspace;

use crate::outline::OpenEditors;

/// The dictionaries of one window and the folders their files live in.
pub struct SpellingState {
    pub speller: Arc<Speller>,
    pub settings_dir: PathBuf,
    pub root: PathBuf,
}

/// The check of the window, which the Settings page reads the words and the source of.
pub struct Spelling(pub Arc<SpellingState>);

impl ui_kit::gpui::Global for Spelling {}

/// The dictionaries of a window. The window tests never ask the checker of the machine:
/// they give their own words through [`use_words`].
fn read(settings_dir: PathBuf, root: PathBuf) -> Speller {
    #[cfg(test)]
    {
        let _ = (settings_dir, root);
        Speller::off()
    }
    #[cfg(not(test))]
    {
        Speller::load(spelling::LANGUAGE, &settings_dir, &root)
    }
}

/// Reads the dictionaries of the user and sets the check of the window.
pub(crate) fn install(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let root = workspace.root().to_path_buf();
    let settings_dir = workspace.settings_dir().to_path_buf();
    let speller = read(settings_dir.clone(), root.clone());
    cx.set_global(Spelling(Arc::new(SpellingState {
        speller: Arc::new(speller),
        settings_dir,
        root,
    })));
}

/// The words the Settings page lists and the line about their source.
pub fn words_and_source(cx: &App) -> (Vec<String>, String) {
    match cx.try_global::<Spelling>() {
        Some(spelling) => (
            spelling.0.speller.user_words(),
            spelling.0.speller.source_line(),
        ),
        None => (Vec::new(), String::new()),
    }
}

/// Checks one editor again. The answer of a text that changed in the meantime is left
/// out, and the text of the same version is checked once.
pub(crate) fn editor_changed(view: &Entity<EditorView>, cx: &mut App) {
    // A check the user turned off in the settings reads no word at all.
    let off = cx.try_global::<InspectionProfile>().is_some_and(|profile| {
        profile
            .0
            .level_of(lang_api::SPELLING_INSPECTION, InspectionLevel::WeakWarning)
            == InspectionLevel::Off
    });
    if off {
        return;
    }
    let Some(state) = cx.try_global::<Spelling>().map(|state| state.0.clone()) else {
        return;
    };
    let Some(version) = view.read(cx).version() else {
        return;
    };
    if !view.update(cx, |view, _| view.claim_spelling(version)) {
        return;
    }
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let path = view.read(cx).path().to_path_buf();
    let visible = view.read(cx).visible_range();
    let support = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone());
    let speller = state.speller.clone();
    let first = {
        let speller = speller.clone();
        let text = text.clone();
        cx.background_executor().spawn(async move {
            let regions = regions_of(&path, &text, support);
            let shown: Vec<Region> = match &visible {
                Some(visible) => regions
                    .iter()
                    .filter(|region| {
                        region.range.start <= visible.end && visible.start <= region.range.end
                    })
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            let misses = speller.check(&text, &shown);
            (misses, regions)
        })
    };
    let rest = text;
    let view = view.clone();
    cx.spawn(async move |cx| {
        // The visible part answers first, so the wave under the eye does not wait for
        // the words of the whole file.
        let (shown, regions) = first.await;
        view.update(cx, |view, cx| {
            view.set_spelling_marks(version, spelling_marks(shown), cx)
        });
        let all = cx
            .background_executor()
            .spawn(async move { speller.check(&rest, &regions) });
        let found = all.await;
        view.update(cx, |view, cx| {
            view.set_spelling_marks(version, spelling_marks(found), cx)
        });
    })
    .detach();
}

/// The marks of the editor from the misses of a check.
fn spelling_marks(found: Vec<spelling::Misspelling>) -> Vec<SpellingMark> {
    found
        .into_iter()
        .map(|miss| SpellingMark {
            range: miss.range,
            word: miss.word,
            suggestions: miss.suggestions,
            declaration: miss.declaration,
        })
        .collect()
}

/// The places the check reads: the language of the file, or the file name itself when
/// the language does not own it.
fn regions_of(
    path: &std::path::Path,
    text: &str,
    support: Option<Arc<dyn lang_api::LanguageSupport>>,
) -> Vec<Region> {
    let query = SpellQuery {
        path: path.to_path_buf(),
        text: text.to_owned(),
    };
    let places = support
        .as_ref()
        .and_then(|support| support.spelling().and_then(|places| places.places(&query)));
    match places {
        Some(places) => places
            .into_iter()
            .map(|place| Region {
                range: place.start..place.end,
                declaration: place.declaration,
            })
            .collect(),
        None => spelling::format_regions(spelling::format_of(path), text)
            .into_iter()
            .map(|range| Region {
                range,
                declaration: false,
            })
            .collect(),
    }
}

/// Checks every open editor again, after a word was saved or a dictionary changed.
pub(crate) fn apply_all(cx: &mut App) {
    for view in OpenEditors::all(cx) {
        view.update(cx, |view, _| view.forget_spelling());
        editor_changed(&view, cx);
    }
}

/// Saves a misspelled word to the dictionary of the project or to the one of the user,
/// then checks the open editors again.
pub(crate) fn save_word(word: &str, project: bool, cx: &mut Context<Workspace>) {
    let Some(state) = cx.try_global::<Spelling>().map(|state| state.0.clone()) else {
        return;
    };
    let word = word.to_owned();
    let path = if project {
        spelling::project_dictionary(&state.root)
    } else {
        spelling::application_dictionary(&state.settings_dir)
    };
    let saved = word.clone();
    let write = cx.background_executor().spawn(async move {
        append_line(&path, &saved)?;
        Ok::<(), std::io::Error>(())
    });
    let spelled = state.clone();
    cx.spawn(async move |workspace, cx| {
        let result = write.await;
        let text = match result {
            Ok(()) => {
                spelled.speller.add_word(&word);
                workspace
                    .update(cx, |_, cx| {
                        apply_all(cx);
                        cx.notify();
                    })
                    .ok();
                format!("'{word}' is saved to the dictionary")
            }
            Err(error) => format!("The dictionary is not saved: {error}"),
        };
        workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user("Spelling", text.clone(), cx);
            })
            .ok();
    })
    .detach();
}

/// Adds a word to the dictionary of the user, from the Settings page.
pub fn add_user_word(cx: &mut App, word: &str) {
    let Some(state) = cx.try_global::<Spelling>().map(|state| state.0.clone()) else {
        return;
    };
    let path = spelling::application_dictionary(&state.settings_dir);
    let saved = word.to_owned();
    let write = cx
        .background_executor()
        .spawn(async move { append_line(&path, &saved) });
    let (settings_dir, root) = (state.settings_dir.clone(), state.root.clone());
    cx.spawn(async move |cx| {
        if write.await.is_ok() {
            cx.update(|cx| load(settings_dir, root, cx));
        }
    })
    .detach();
}

/// Removes a word from the dictionary of the user, from the Settings page.
pub fn remove_user_word(cx: &mut App, word: &str) {
    let Some(state) = cx.try_global::<Spelling>().map(|state| state.0.clone()) else {
        return;
    };
    let path = spelling::application_dictionary(&state.settings_dir);
    let word = word.to_owned();
    let removed = word.clone();
    let write = cx.background_executor().spawn(async move {
        let text = std::fs::read_to_string(&path)?;
        let kept: String = text
            .lines()
            .filter(|line| !line.trim().eq_ignore_ascii_case(&removed))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, kept)
    });
    let (settings_dir, root) = (state.settings_dir.clone(), state.root.clone());
    cx.spawn(async move |cx| {
        if write.await.is_ok() {
            cx.update(|cx| load(settings_dir, root, cx));
        }
    })
    .detach();
}

/// Reads the dictionaries again, after a page of the settings changed one.
fn load(settings_dir: PathBuf, root: PathBuf, cx: &mut App) {
    let speller = read(settings_dir.clone(), root.clone());
    cx.set_global(Spelling(Arc::new(SpellingState {
        speller: Arc::new(speller),
        settings_dir,
        root,
    })));
    apply_all(cx);
}

/// Appends a word to a dictionary file unless the file has it already.
fn append_line(path: &std::path::Path, word: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(word))
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(word);
    text.push('\n');
    std::fs::write(path, text)
}

/// Replaces the dictionaries with the given words, for the window tests.
#[cfg(test)]
pub(crate) fn use_words(cx: &mut App, words: &[&str]) {
    let Some(existing) = cx.try_global::<Spelling>().map(|state| state.0.clone()) else {
        return;
    };
    let speller = Speller::with_words(words.iter().map(|word| (*word).to_owned()));
    cx.set_global(Spelling(Arc::new(SpellingState {
        speller: Arc::new(speller),
        settings_dir: existing.settings_dir.clone(),
        root: existing.root.clone(),
    })));
    apply_all(cx);
}

/// Whether the check of this machine has a dictionary at all, for the settings page.
pub fn enabled(cx: &App) -> bool {
    cx.try_global::<Spelling>()
        .is_some_and(|spelling| spelling.0.speller.enabled())
}
