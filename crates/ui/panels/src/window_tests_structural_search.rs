//! Search Structurally and Replace Structurally without a window: the template, the
//! filters of its variables, the ready templates and the places in the Find window. A
//! language of the test answers the search, so the engine of the real language is not
//! needed here.

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::Languages;
use lang_api::{
    LanguageFn, LanguageId, LanguageSupport, Queries, StructuralEdit, StructuralMatch,
    StructuralQuery, StructuralSearchProvider, StructuralTemplate, StructuralVariable,
    SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::structural::{ActiveStructuralSearch, StructuralSearch};
use crate::usages::ActiveUsages;
use crate::window_tests::{
    click, draw, editor_of, open_palette, open_path, press, scratch, type_text,
    wait_for_file_index, wait_for_signal,
};

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct TestSyntax(Queries);

impl SyntaxProvider for TestSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A text search that knows `prefix($name$)` and `prefix($name$)` with a suffix. It is
/// enough for the window: the real matcher has its own tests.
struct TestSearch;

fn variables_of(pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = pattern;
    while let Some(at) = rest.find('$') {
        let after = &rest[at + 1..];
        let Some(end) = after.find('$') else {
            break;
        };
        let name = &after[..end];
        if !name.is_empty() && !names.iter().any(|kept| kept == name) {
            names.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    names
}

/// The places of a single-variable template: the literal before and after the variable.
fn places(pattern: &str, text: &str) -> Vec<(Range<usize>, BTreeMap<String, String>)> {
    let names = variables_of(pattern);
    let [name] = names.as_slice() else {
        return Vec::new();
    };
    let marker = format!("${name}$");
    let Some((left, right)) = pattern.split_once(&marker) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find(left) {
        let start = at + offset;
        let value_start = start + left.len();
        let Some(value_offset) = text[value_start..].find(right) else {
            break;
        };
        let value_end = value_start + value_offset;
        let value = &text[value_start..value_end];
        if value.contains('\n') {
            at = value_start;
            continue;
        }
        let end = value_end + right.len();
        let mut captures = BTreeMap::new();
        captures.insert(name.clone(), value.to_owned());
        found.push((start..end, captures));
        at = end;
    }
    found
}

fn render(replacement: &str, captures: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        match captures.get(name) {
            Some(value) => out.push_str(value),
            None => {
                out.push('$');
                out.push_str(name);
                out.push('$');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn passes(query: &StructuralQuery, captures: &BTreeMap<String, String>) -> bool {
    for variable in &query.variables {
        let Some(value) = captures.get(&variable.name) else {
            continue;
        };
        if let Some(text) = &variable.text {
            let (inverted, pattern) = match text.strip_prefix('!') {
                Some(pattern) => (true, pattern),
                None => (false, text.as_str()),
            };
            if value.contains(pattern) == inverted {
                return false;
            }
        }
    }
    true
}

impl StructuralSearchProvider for TestSearch {
    fn templates(&self) -> Vec<StructuralTemplate> {
        vec![StructuralTemplate {
            id: "system_out_println".to_owned(),
            name: "System.out.println".to_owned(),
            description: "A call of println on the standard output".to_owned(),
            pattern: "System.out.println($a$)".to_owned(),
            replacement: Some("log.info($a$)".to_owned()),
            variables: vec![StructuralVariable::named("a")],
        }]
    }

    fn variables(&self, pattern: &str) -> Result<Vec<String>, String> {
        Ok(variables_of(pattern))
    }

    fn find(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralMatch>, String> {
        Ok(places(&query.pattern, text)
            .into_iter()
            .filter(|(_, captures)| passes(query, captures))
            .map(|(range, captures)| StructuralMatch {
                start: range.start,
                end: range.end,
                text: text[range.clone()].to_owned(),
                replacement: query
                    .replacement
                    .as_ref()
                    .map(|replacement| render(replacement, &captures)),
                notes: Vec::new(),
            })
            .collect())
    }

    fn replace(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralEdit>, String> {
        Ok(self
            .find(text, query)?
            .into_iter()
            .filter_map(|found| {
                Some(StructuralEdit {
                    start: found.start,
                    end: found.end,
                    replacement: found.replacement?,
                })
            })
            .collect())
    }
}

struct TestLanguage {
    search: TestSearch,
    syntax: TestSyntax,
}

impl LanguageSupport for TestLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("structural-test")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn structural_search(&self) -> Option<&dyn StructuralSearchProvider> {
        Some(&self.search)
    }
}

fn language() -> Arc<TestLanguage> {
    Arc::new(TestLanguage {
        search: TestSearch,
        syntax: TestSyntax(Queries::default()),
    })
}

fn start(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, PathBuf, PathBuf, WindowHandle<Workspace>) {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    let alpha = root.join("src").join("Alpha.txt");
    let beta = root.join("src").join("Beta.txt");
    fs::write(
        &alpha,
        "class Alpha {\n    void run() {\n        System.out.println(one);\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        &beta,
        "class Beta {\n    void run() {\n        System.out.println(two);\n    }\n}\n",
    )
    .unwrap();
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let support = language();
    cx.update(|cx| {
        cx.set_global(editor_view::Semantic(support));
        cx.set_global(Languages {
            resolve: |path| {
                path.extension()
                    .is_some_and(|extension| extension == "txt")
                    .then(dockerfile_syntax::config)
            },
            templates: |_| None,
        });
    });
    wait_for_file_index(cx);
    draw(cx, window);
    (root, alpha, beta, window)
}

fn dialog(cx: &TestAppContext) -> Entity<StructuralSearch> {
    cx.read(|cx| {
        cx.global::<ActiveStructuralSearch>()
            .0
            .clone()
            .expect("the structural search dialog is open")
    })
}

fn results(cx: &TestAppContext) -> String {
    let hub = cx.read(|cx| cx.global::<ActiveUsages>().0.clone());
    hub.read_with(cx, |hub, cx| hub.panel().read(cx).export_text())
}

fn result_count(cx: &TestAppContext) -> usize {
    let hub = cx.read(|cx| cx.global::<ActiveUsages>().0.clone());
    hub.read_with(cx, |hub, cx| hub.panel().read(cx).items().len())
}

#[ui_kit::gpui::test]
fn the_template_finds_calls_and_enter_opens_the_place(cx: &mut TestAppContext) {
    let (_root, alpha, _beta, window) = start(cx, "structural-find");
    open_palette(cx, window, "workspace.search_structurally");
    wait_for_signal(cx, "the structural search dialog", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveStructuralSearch>()
                .is_some_and(|it| it.0.is_some())
        })
    });
    type_text(cx, window, "System.out.println($a$)");
    wait_for_signal(cx, "the two calls", |cx| result_count(cx) == 2);
    draw(cx, window);
    assert!(
        results(cx).contains("Alpha.txt") && results(cx).contains("Beta.txt"),
        "{}",
        results(cx)
    );
    assert_eq!(
        dialog(cx).read_with(cx, |dialog, _| dialog.variable_names()),
        ["a"]
    );
    let status = dialog(cx).read_with(cx, |dialog, _| dialog.status_text().map(str::to_owned));
    assert_eq!(status.as_deref(), Some("2 matches in 2 files"));

    // Escape closes the dialog; the Find window keeps the places and Enter opens one.
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        cx.read(|cx| cx
            .try_global::<ActiveStructuralSearch>()
            .is_some_and(|it| it.0.is_none())),
        "Escape closes the dialog"
    );
    click(cx, window, "item-row-0");
    press(cx, window, "enter");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap(),
        Some(alpha.clone())
    );
    let view = editor_of(cx, window, &alpha);
    assert_eq!(view.read_with(cx, |view, _| view.caret()).0, 2);
}

#[ui_kit::gpui::test]
fn a_filter_narrows_the_variable_and_a_ready_template_fills_the_dialog(cx: &mut TestAppContext) {
    let (_root, _alpha, _beta, window) = start(cx, "structural-filter");
    open_palette(cx, window, "workspace.search_structurally");
    wait_for_signal(cx, "the structural search dialog", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveStructuralSearch>()
                .is_some_and(|it| it.0.is_some())
        })
    });
    type_text(cx, window, "System.out.println($a$)");
    wait_for_signal(cx, "the two calls", |cx| result_count(cx) == 2);
    draw(cx, window);

    // The `one` call only passes a text filter of the variable.
    click(cx, window, "structural-var-0-text-box");
    type_text(cx, window, "one");
    wait_for_signal(cx, "the filtered call", |cx| result_count(cx) == 1);
    assert!(results(cx).contains("println(one)"), "{}", results(cx));

    // A ready template of the language replaces the template and its variables.
    click(cx, window, "structural-template-system_out_println");
    wait_for_signal(cx, "the ready template", |cx| {
        dialog(cx).read_with(cx, |dialog, _| {
            dialog.template_text() == "System.out.println($a$)"
        })
    });
    wait_for_signal(cx, "the places of the ready template", |cx| {
        result_count(cx) >= 1
    });
}

#[ui_kit::gpui::test]
fn replace_shows_the_preview_writes_the_files_and_ctrl_z_takes_it_back(cx: &mut TestAppContext) {
    let (_root, alpha, beta, window) = start(cx, "structural-replace");
    open_path(cx, window, &alpha);
    open_palette(cx, window, "workspace.replace_structurally");
    wait_for_signal(cx, "the replace dialog", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveStructuralSearch>()
                .is_some_and(|it| it.0.is_some())
        })
    });
    type_text(cx, window, "System.out.println($a$)");
    wait_for_signal(cx, "the two calls", |cx| result_count(cx) == 2);
    // Tab moves to the replacement; the places preview the new text.
    press(cx, window, "tab");
    type_text(cx, window, "log.info($a$)");
    wait_for_signal(cx, "the preview", |cx| {
        results(cx).contains("log.info(one)") && results(cx).contains("log.info(two)")
    });
    draw(cx, window);

    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the replaced files", |_| {
        fs::read_to_string(&alpha).is_ok_and(|text| text.contains("log.info(one)"))
            && fs::read_to_string(&beta).is_ok_and(|text| text.contains("log.info(two)"))
    });
    let view = editor_of(cx, window, &alpha);
    assert!(
        view.read_with(cx, |view, _| view.text())
            .unwrap()
            .contains("log.info(one)")
    );

    // The replacement of the open file is one undo step.
    press(cx, window, "escape");
    draw(cx, window);
    click(cx, window, "editor-body-0");
    press(cx, window, "ctrl-z");
    let text = view.read_with(cx, |view, _| view.text()).unwrap();
    assert!(text.contains("System.out.println(one)"), "{text}");
    assert!(!text.contains("log.info(one)"), "{text}");
}
