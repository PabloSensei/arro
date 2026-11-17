//! History for Selection, History for Method, the commit of a project with a submodule
//! and the root filter of the log. Every repository is a scratch folder of real git.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::Semantic;
use lang_api::{
    LanguageFn, LanguageId, LanguageSupport, Queries, Symbol, SymbolKind, SymbolProvider,
    SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::git_history::ActiveSelectionHistory;
use crate::git_log::ActiveGitLog;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{draw, editor_of, open_path, open_window, press, scratch};
use crate::window_tests_git::{git, git_state, wait_for, wait_for_repository};

fn init(root: &Path) {
    fs::create_dir_all(root).unwrap();
    let git = git();
    git.run(root, ["init", "-q", "-b", "main"]).unwrap();
    for (key, value) in [
        ("user.name", "Window Test"),
        ("user.email", "window-test@example.com"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
        ("protocol.file.allow", "always"),
    ] {
        git.run(root, ["config", key, value]).unwrap();
    }
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn commit(root: &Path, message: &str) {
    let git = git();
    git.run(root, ["add", "-A"]).unwrap();
    git.run(root, ["commit", "-q", "-m", message]).unwrap();
}

/// A repository whose file went through three commits: the base, a change of the second
/// line and a change of the fourth one.
fn lines_project(name: &str) -> PathBuf {
    let root = scratch(name);
    init(&root);
    write(&root, "f.txt", "one\ntwo\nthree\nfour\nfive\n");
    commit(&root, "base");
    write(&root, "f.txt", "one\nTWO\nthree\nfour\nfive\n");
    commit(&root, "second");
    write(&root, "f.txt", "one\nTWO\nthree\nFOUR\nfive\n");
    commit(&root, "third");
    root
}

/// A parent whose submodule is a repository of its own; both have a change to commit.
fn submodule_project(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let sub = root.join("sub");
    init(&sub);
    write(&sub, "inside.txt", "inside\n");
    commit(&sub, "sub base");

    let parent = root.join("parent");
    init(&parent);
    write(&parent, "main.txt", "main\n");
    commit(&parent, "base");
    git()
        .run(
            &parent,
            [
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub.to_str().unwrap(),
                "sub",
            ],
        )
        .unwrap();
    let clone = parent.join("sub");
    for (key, value) in [
        ("user.name", "Window Test"),
        ("user.email", "window-test@example.com"),
    ] {
        git().run(&clone, ["config", key, value]).unwrap();
    }
    commit(&parent, "add submodule");
    write(&parent, "main.txt", "changed\n");
    write(&parent.join("sub"), "inside.txt", "changed\n");
    (parent.clone(), parent.join("sub"))
}

fn selection_history(
    cx: &TestAppContext,
) -> ui_kit::gpui::Entity<crate::git_history::SelectionHistoryPanel> {
    cx.read(|cx| cx.global::<ActiveSelectionHistory>().0.clone())
}

fn commit_panel(cx: &TestAppContext) -> ui_kit::gpui::Entity<crate::commit::CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

fn log_panel(cx: &TestAppContext) -> ui_kit::gpui::Entity<crate::git_log::GitLogPanel> {
    cx.read(|cx| cx.global::<ActiveGitLog>().0.clone())
}

fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, command: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(command, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn history_for_selection_lists_only_the_commits_of_the_lines(cx: &mut TestAppContext) {
    let root = lines_project("selection-history");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("f.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");

    run_command(cx, window, "git.show_selection_history");
    draw(cx, window);
    let panel = selection_history(cx);
    wait_for(cx, "the history of the lines", |cx| {
        !panel.read_with(cx, |panel, _| panel.subjects().is_empty())
    });

    let subjects = panel.read_with(cx, |panel, _| panel.subjects());
    assert_eq!(subjects, ["second", "base"]);
    assert_eq!(panel.read_with(cx, |panel, _| panel.lines()), (2, 3));
    let diff = panel.read_with(cx, |panel, _| panel.diff_text());
    assert!(diff.contains("TWO"), "{diff}");
    assert!(diff.contains("two"), "{diff}");

    // The unsaved change of the selected line comes first.
    run_command(cx, window, "git.show_selection_history");
    let text = view.read_with(cx, |view, _| view.text()).unwrap();
    let start = text.find("TWO").unwrap();
    view.update(cx, |view, cx| {
        view.replace_text(start..start + 3, "LOCAL", cx);
    });
    run_command(cx, window, "git.show_selection_history");
    wait_for(cx, "the local changes", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .subjects()
                .first()
                .is_some_and(|first| first == "Local Changes")
        })
    });
    let subjects = panel.read_with(cx, |panel, _| panel.subjects());
    assert_eq!(subjects[0], "Local Changes");
    assert!(subjects.contains(&"second".to_owned()));
    let diff = panel.read_with(cx, |panel, _| panel.diff_text());
    assert!(diff.contains("LOCAL"), "{diff}");
}

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

/// The one method of the test file, with the range of its body.
struct MethodSymbols;

impl SymbolProvider for MethodSymbols {
    fn file_symbols(&self, _: &Path, text: &str) -> Vec<Symbol> {
        let Some(start) = text.find("void m") else {
            return Vec::new();
        };
        let Some(open) = text[start..].find('{').map(|at| at + start) else {
            return Vec::new();
        };
        let mut depth = 0;
        let mut end = text.len();
        for (at, character) in text[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + at + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        vec![Symbol {
            name: "m".to_owned(),
            kind: SymbolKind::Method,
            container: Some("A".to_owned()),
            depth: 1,
            range: start..end,
            name_range: start..start + "void m".len(),
        }]
    }
}

static METHOD_SYMBOLS: MethodSymbols = MethodSymbols;

struct MethodLanguage {
    syntax: NoSyntax,
}

impl LanguageSupport for MethodLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-method")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        Some(&METHOD_SYMBOLS)
    }
}

#[gpui::test]
fn history_for_method_uses_the_declaration_of_the_language(cx: &mut TestAppContext) {
    let root = scratch("method-history");
    init(&root);
    write(
        &root,
        "A.txt",
        "class A {\n  void m() {\n    beta\n  }\n}\n",
    );
    commit(&root, "base");
    write(
        &root,
        "A.txt",
        "class A {\n  void m() {\n    BETA\n  }\n}\n",
    );
    commit(&root, "second");
    write(
        &root,
        "A.txt",
        "class A {\n  void m() {\n    BETA\n  }\n}\n// tail\n",
    );
    commit(&root, "third");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    cx.update(|cx| {
        cx.set_global(Semantic(Arc::new(MethodLanguage {
            syntax: NoSyntax(Queries::default()),
        })));
    });
    let path = root.join("A.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(2, 4, cx));

    run_command(cx, window, "git.show_method_history");
    draw(cx, window);
    let panel = selection_history(cx);
    wait_for(cx, "the history of the method", |cx| {
        !panel.read_with(cx, |panel, _| panel.subjects().is_empty())
    });

    let subjects = panel.read_with(cx, |panel, _| panel.subjects());
    assert_eq!(subjects, ["second", "base"]);
    assert_eq!(panel.read_with(cx, |panel, _| panel.lines()), (2, 4));
}

#[gpui::test]
fn commit_of_a_project_with_a_submodule_makes_one_commit_in_each_root(cx: &mut TestAppContext) {
    let (root, sub) = submodule_project("submodule-commit");
    let _window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the roots of the project", |cx| {
        state.read_with(cx, |state, _| state.roots().len() == 2)
    });
    let panel = commit_panel(cx);
    wait_for(cx, "the changed files", |cx| {
        !state.read_with(cx, |state, _| state.status().files.is_empty())
    });
    let headings = panel.read_with(cx, |panel, cx| panel.root_headings(cx));
    assert_eq!(headings.len(), 2);
    panel.update(cx, |panel, cx| panel.set_message("both roots", cx));
    panel.update(cx, |panel, cx| panel.commit(false, cx));
    wait_for(cx, "the commits", |cx| {
        state.read_with(cx, |state, _| state.busy().is_none())
            && count(&root) == 3
            && count(&sub) == 2
    });

    assert_eq!(count(&root), 3);
    assert_eq!(count(&sub), 2);
    let recorded = String::from_utf8_lossy(&git().run(&root, ["rev-parse", "HEAD:sub"]).unwrap())
        .trim()
        .to_owned();
    let working = String::from_utf8_lossy(&git().run(&sub, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    assert_eq!(recorded, working, "the parent records the new commit");
    for repository in [&root, &sub] {
        let subject = git().run(repository, ["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&subject).trim(), "both roots");
    }
}

fn count(root: &Path) -> usize {
    let output = git()
        .run(root, ["rev-list", "--count", "HEAD"])
        .unwrap_or_default();
    String::from_utf8_lossy(&output).trim().parse().unwrap_or(0)
}

#[gpui::test]
fn the_log_shows_every_root_and_filters_one(cx: &mut TestAppContext) {
    let (root, sub) = submodule_project("submodule-log");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the roots of the project", |cx| {
        state.read_with(cx, |state, _| state.roots().len() == 2)
    });
    run_command(cx, window, "git.show_log");
    let panel = log_panel(cx);
    wait_for(cx, "the log", |cx| {
        !panel.read_with(cx, |panel, _| panel.commits().is_empty())
    });

    let seen: HashSet<String> =
        panel.read_with(cx, |panel, _| panel.shown_roots().into_iter().collect());
    assert_eq!(panel.read_with(cx, |panel, _| panel.roots()).len(), 2);
    assert!(seen.contains(""), "{seen:?}");
    assert!(seen.contains("sub"), "{seen:?}");

    panel.update(cx, |panel, cx| panel.set_root(Some("sub".to_owned()), cx));
    wait_for(cx, "the log of the submodule", |cx| {
        panel.read_with(cx, |panel, _| {
            !panel.commits().is_empty() && panel.shown_roots().into_iter().all(|root| root == "sub")
        })
    });
    let sub_commits: HashSet<String> = panel.read_with(cx, |panel, _| {
        panel
            .commits()
            .into_iter()
            .map(|commit| commit.id.clone())
            .collect()
    });
    let expected: HashSet<String> = {
        let output = git().run(&sub, ["log", "--format=%H"]).unwrap_or_default();
        String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(sub_commits, expected);
}
