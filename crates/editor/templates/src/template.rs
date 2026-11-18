//! What a language declares: its templates and where in the tree each kind fits.

use std::ops::Range;
use std::sync::OnceLock;

use tree_sitter::{Language, Query, Tree};

/// A context that every place fits.
pub const ANY: &str = "any";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Typed as a word, expanded by Tab or picked from the list.
    Abbreviation,
    /// Typed after an expression and a dot: `$EXPR$` is that expression.
    Postfix,
    /// Picked from the Surround With list: `$SELECTION$` is the selected lines.
    Surround,
    /// Picked from the Surround with Live Template list, next to [`Kind::Surround`]:
    /// `$SELECTION$` is the selected lines.
    SurroundLive,
}

/// One template. The body names fields as `$NAME$`, `$$` is a dollar sign, a tab at the
/// start of a line is one indent unit of the language. `$END$` is where the caret ends,
/// `$SELECTION$` and `$EXPR$` are filled in, every other name is a field. A field that
/// occurs several times is typed once.
#[derive(Clone, Copy, Debug)]
pub struct Template {
    pub key: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub body: &'static str,
    /// Fields in the order Tab visits them, with the text they start with. Fields not
    /// listed come after, empty, in the order of the body.
    pub fields: &'static [(&'static str, &'static str)],
    /// Context names from [`TemplateLanguage::contexts`], or [`ANY`].
    pub contexts: &'static [&'static str],
}

impl Template {
    pub fn fits(&self, context: &str) -> bool {
        self.contexts
            .iter()
            .any(|fits| *fits == ANY || *fits == context)
    }
}

/// What the language knows about the expression in front of a postfix key. An unknown
/// kind never hides a template: only a proven one can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Receiver {
    /// The language proved the kind.
    pub known: bool,
    /// The value is a reference, so it can be null.
    pub reference: bool,
    /// The value is a boolean.
    pub boolean: bool,
    /// The value is an array or a collection of elements.
    pub sequence: bool,
    /// The value is text.
    pub text: bool,
    /// The value is a number.
    pub number: bool,
}

/// How a language reads the expression a postfix template follows and decides whether
/// the template fits it.
#[derive(Clone, Copy)]
pub struct PostfixRules {
    /// The kind of the expression in `range` of `text`, or an unknown one.
    pub receiver: fn(&str, Option<&Tree>, Range<usize>) -> Receiver,
    /// Whether `template` expands an expression of this kind.
    pub applies: fn(&Template, Receiver) -> bool,
}

/// The templates of one language.
pub struct TemplateLanguage {
    pub templates: &'static [Template],
    /// A query over the grammar. Each capture names the context of the text inside the
    /// captured node; the innermost one around the caret wins. Outside all of them the
    /// context is `other`.
    pub contexts: &'static str,
    pub grammar: fn() -> Language,
    pub indent_unit: &'static str,
    /// Words that belong to the expression in front of a postfix key, like a keyword that
    /// creates an object.
    pub receiver_prefixes: &'static [&'static str],
    /// The kinds of the expressions before postfix keys and the templates that fit them;
    /// `None` offers every postfix template everywhere.
    pub postfix: Option<PostfixRules>,
    pub query: OnceLock<Option<Query>>,
    /// Reads the names around the caret that `className()`, `methodName()` and
    /// `packageName()` give; `None` leaves them empty.
    pub declarations: Option<crate::DeclarationsProvider>,
}

impl TemplateLanguage {
    /// The compiled context query; `None` when it does not fit the grammar or when the
    /// language names no context: every place is `other` there.
    pub fn query(&self) -> Option<&Query> {
        let query = self.query.get_or_init(|| {
            if self.contexts.trim().is_empty() {
                return None;
            }
            Query::new(&(self.grammar)(), self.contexts).ok()
        });
        query.as_ref()
    }

    /// Templates of `kind` that fit `context`, in the order they are declared.
    pub fn fitting<'a>(
        &'a self,
        kind: Kind,
        context: &'a str,
    ) -> impl Iterator<Item = &'a Template> + 'a {
        let templates = self.templates.iter();
        templates.filter(move |template| template.kind == kind && template.fits(context))
    }

    pub fn find(&self, kind: Kind, key: &str, context: &str) -> Option<&Template> {
        let mut templates = self.templates.iter();
        templates
            .find(|template| template.kind == kind && template.key == key && template.fits(context))
    }
}
