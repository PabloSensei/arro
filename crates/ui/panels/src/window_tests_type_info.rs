//! Ctrl+Shift+P shows one line under the caret: the type the language gives for the expression
//! at the caret, or for the selection. It is not the first line of the hover answer. The fake
//! language gives the place of its answer and its own type, so the note shows what it said.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::Semantic;
use lang_api::{
    Answer, Confidence, DocumentSpot, ExpressionTypePlace, HoverProvider, HoverText, LanguageFn,
    LanguageId, LanguageSupport, Provider, Queries, SourceId, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::window_tests::{draw, editor_of, open_path, open_window, press, sample_project};

const SOURCE: SourceId = SourceId::new("type-info-server");

/// The first line of the server's answer: the declaration with its place. The note must not
/// show it.
const DECLARATION: &str = "```text\nPage<Owner> paginated - org.example.owner.OwnerController.addPaginationModel(int, Model, Page<Owner>)\n```\n\nThe parameter of the method.\n";

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language that answers the type of a name as `type at N`, where N is the place it asks
/// the hover at: the end of the expression. A language that knows no type says so when
/// `typed` is false.
struct TypeLanguage {
    syntax: NoSyntax,
    typed: bool,
}

impl TypeLanguage {
    fn new(typed: bool) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            typed,
        })
    }
}

impl Provider<DocumentSpot, HoverText> for TypeLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<HoverText>) {
        let _ = reply.send(Answer::new(
            HoverText(DECLARATION.to_owned()),
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl LanguageSupport for TypeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("type-info")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn hover(&self) -> Option<&HoverProvider> {
        Some(self)
    }

    fn expressions_at(&self, text: &str, offset: usize) -> Vec<Range<usize>> {
        word_at(text, offset).into_iter().collect()
    }

    fn expression_type_place(
        &self,
        text: &str,
        expression: Range<usize>,
    ) -> Option<ExpressionTypePlace> {
        let word = text.get(expression.clone())?;
        if word.chars().all(|character| character.is_ascii_digit()) {
            return Some(ExpressionTypePlace::Known("int".to_owned()));
        }
        Some(ExpressionTypePlace::Hover(expression.end))
    }

    fn expression_type_from_hover(&self, _: &str, offset: usize, _: &str) -> Option<String> {
        self.typed.then(|| format!("type at {offset}"))
    }
}

/// The word under `offset` of an ASCII text: letters, digits and underscores.
fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let start = text[..offset]
        .rfind(|character: char| !is_word(character))
        .map_or(0, |index| index + 1);
    let end = text[offset..]
        .find(|character: char| !is_word(character))
        .map_or(text.len(), |index| offset + index);
    (start < end).then_some(start..end)
}

/// Opens `text` as the file of a window whose language is the fake one.
fn open_fake(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
    typed: bool,
) -> (WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    let support: Arc<dyn LanguageSupport> = TypeLanguage::new(typed);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Semantic(support));
    });
    open_path(cx, window, &path);
    (window, path)
}

fn note_of(cx: &TestAppContext, window: WindowHandle<Workspace>, path: &Path) -> Option<String> {
    editor_of(cx, window, path).read_with(cx, |view, _| view.note_text())
}

#[ui_kit::gpui::test]
fn ctrl_shift_p_shows_the_type_of_the_language_not_the_first_line_of_the_answer(
    cx: &mut TestAppContext,
) {
    let (window, path) = open_fake(cx, "type-info-name", "value\n", true);

    press(cx, window, "ctrl-shift-p");
    draw(cx, window);

    assert_eq!(
        note_of(cx, window, &path),
        Some("type at 5".to_owned()),
        "the note shows the type the language gives, in one line"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_p_says_no_type_here_when_the_language_knows_no_type(cx: &mut TestAppContext) {
    let (window, path) = open_fake(cx, "type-info-untyped", "value\n", false);

    press(cx, window, "ctrl-shift-p");
    draw(cx, window);

    assert_eq!(
        note_of(cx, window, &path),
        Some("No type here".to_owned()),
        "a name the language gives no type for says so"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_p_asks_about_the_selected_expression(cx: &mut TestAppContext) {
    let (window, path) = open_fake(cx, "type-info-selection", "alpha beta\n", true);

    // The caret is on the first word; the selection takes the whole line, which the caret alone
    // would not ask about.
    press(cx, window, "shift-end");
    press(cx, window, "ctrl-shift-p");
    draw(cx, window);

    assert_eq!(
        note_of(cx, window, &path),
        Some("type at 10".to_owned()),
        "the selected expression is the one the language answers about"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_p_takes_a_literal_type_from_the_text(cx: &mut TestAppContext) {
    let (window, path) = open_fake(cx, "type-info-literal", "count 42\n", true);

    press(cx, window, "right right right right right right");
    press(cx, window, "ctrl-shift-p");
    draw(cx, window);

    assert_eq!(
        note_of(cx, window, &path),
        Some("int".to_owned()),
        "a literal has its type without the server"
    );
}
