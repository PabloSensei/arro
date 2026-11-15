//! The badges of the gutter that open the beans of an injection model and the Beans
//! window, on a language that answers from memory. A real project reads the model in
//! the language crate; here the test writes the model and checks what the window does
//! with it: the marks, the jump, the list of several candidates, the checks and the tree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::Cancel;
use lang_api::{
    Answer, Bean, BeanModel, BeanProvider, BeanQuery, BeanWarning, Confidence, InjectionPlace,
    InjectionPoint, LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SourceId,
    SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{BEANS_TOOL, Workspace};

use crate::beans::{ActiveBeanHub, ActiveBeans};
use crate::problems::ActiveProblems;
use crate::window_tests::{
    active_path, bounds_of, click, click_gutter, draw, editor_of, open_path, open_window, scratch,
    type_text, wait_for_signal,
};

const SOURCE: SourceId = SourceId::new("memory-beans");

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

/// A language that gives the model of the beans it was made with once.
struct BeanLanguage {
    syntax: NoSyntax,
    model: BeanModel,
}

impl BeanLanguage {
    fn new(model: BeanModel) -> Self {
        Self {
            syntax: NoSyntax(Queries::default()),
            model,
        }
    }
}

impl Provider<BeanQuery, BeanModel> for BeanLanguage {
    fn start(&self, _: BeanQuery, _: Cancel, mut reply: lang_api::Reply<BeanModel>) {
        let _ = reply.send(Answer::new(self.model.clone(), Confidence::Certain, SOURCE));
    }
}

impl BeanProvider for BeanLanguage {
    fn sources_changed(&self) {}
}

impl LanguageSupport for BeanLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-beans")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn module_of(&self, _: &Path) -> Option<String> {
        Some("demo".to_owned())
    }

    fn beans(&self) -> Option<&dyn BeanProvider> {
        Some(self)
    }
}

fn write(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn place(label: &str, file: &Path, offset: usize) -> InjectionPlace {
    InjectionPlace {
        label: label.to_owned(),
        file: file.to_path_buf(),
        offset,
    }
}

fn bean(
    name: &str,
    type_name: &str,
    stereotype: &str,
    file: &Path,
    line: u32,
    offset: usize,
    usages: Vec<InjectionPlace>,
) -> Bean {
    Bean {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        qualified_type: format!("demo.{type_name}"),
        stereotype: stereotype.to_owned(),
        primary: false,
        file: file.to_path_buf(),
        line,
        offset,
        usages,
    }
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
    model: BeanModel,
) -> (WindowHandle<Workspace>, PathBuf) {
    let root = scratch(name);
    for (file, text) in files {
        write(&root, file, text);
    }
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    cx.run_until_parked();
    (window, root)
}

/// A bean, a point and a factory method of three files, all of them with a usage.
fn three_files(root: &Path) -> (BeanModel, PathBuf, PathBuf, PathBuf) {
    let owners = root.join("Owners.txt");
    let repo = root.join("Repo.txt");
    let clock = root.join("Clock.txt");
    let repo_at = 6;
    let clock_at = 19;
    let model = BeanModel {
        beans: vec![
            bean(
                "owners",
                "Owners",
                "@Component",
                &owners,
                0,
                6,
                vec![place("owners — Repo.txt", &repo, repo_at)],
            ),
            bean(
                "repo",
                "Repo",
                "@Component",
                &repo,
                0,
                repo_at,
                vec![place("repo — Owners.txt", &owners, 21)],
            ),
            bean(
                "clock",
                "Clock",
                "@Bean",
                &clock,
                3,
                clock_at,
                vec![place("clock — Owners.txt", &owners, 21)],
            ),
            bean("conf", "Conf", "@Configuration", &clock, 1, 6, Vec::new()),
        ],
        injections: vec![InjectionPoint {
            type_name: "Repo".to_owned(),
            qualified_type: "demo.Repo".to_owned(),
            file: owners.clone(),
            line: 1,
            offset: 21,
            range: 15..19,
            candidates: vec![place("repo", &repo, repo_at)],
        }],
        warnings: Vec::new(),
    };
    (model, owners, repo, clock)
}

const OWNERS: &str = "class Owners {\n    Repo repo;\n}\n";
const REPO: &str = "class Repo {}\n";
const CLOCK: &str =
    "@Configuration\nclass Conf {\n    @Bean\n    Clock clock() { return null; }\n}\n";

fn marks(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Vec<(u32, String)> {
    let view = editor_of(cx, window, path);
    view.read_with(cx, |view, _| {
        view.place_marks()
            .iter()
            .map(|mark| (mark.line, mark.hint.to_string()))
            .collect()
    })
}

#[ui_kit::gpui::test]
fn the_gutter_marks_a_bean_a_point_and_a_factory_method(cx: &mut TestAppContext) {
    let root = scratch("beans-marks");
    write(&root, "Owners.txt", OWNERS);
    write(&root, "Repo.txt", REPO);
    write(&root, "Clock.txt", CLOCK);
    let (model, owners, _repo, clock) = three_files(&root);
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    open_path(cx, window, &owners);
    wait_for_signal(cx, "the badge of the bean", |cx| {
        marks(cx, window, &owners)
            .iter()
            .any(|(line, _)| *line == 0)
    });
    let found = marks(cx, window, &owners);
    assert!(
        found.contains(&(0, "Navigate to usages of the bean".to_owned())),
        "{found:?}"
    );
    assert!(
        found.contains(&(1, "Navigate to autowired candidates".to_owned())),
        "{found:?}"
    );
    open_path(cx, window, &clock);
    wait_for_signal(cx, "the badge of the factory method", |cx| {
        marks(cx, window, &clock).iter().any(|(line, _)| *line == 3)
    });
    let found = marks(cx, window, &clock);
    assert!(
        found.contains(&(1, "Navigate to usages of the bean".to_owned())),
        "{found:?}"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_a_point_goes_to_its_only_candidate(cx: &mut TestAppContext) {
    let root = scratch("beans-click");
    let (model, owners, repo, _clock) = three_files(&root);
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    write(&root, "Owners.txt", OWNERS);
    write(&root, "Repo.txt", REPO);
    open_path(cx, window, &owners);
    wait_for_signal(cx, "the badge of the point", |cx| {
        marks(cx, window, &owners)
            .iter()
            .any(|(line, _)| *line == 1)
    });
    draw(cx, window);
    click_gutter(cx, window, 0, 1);
    let path = active_path(cx, window).expect("the candidate opens");
    assert_eq!(path, repo);
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret_line()), Some(0));
}

#[ui_kit::gpui::test]
fn a_click_on_a_bean_opens_the_list_of_its_usages(cx: &mut TestAppContext) {
    let root = scratch("beans-usages");
    write(&root, "Owners.txt", OWNERS);
    write(&root, "Repo.txt", REPO);
    write(&root, "Clock.txt", CLOCK);
    let (model, owners, repo, _clock) = three_files(&root);
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    open_path(cx, window, &owners);
    wait_for_signal(cx, "the badge of the bean", |cx| {
        marks(cx, window, &owners)
            .iter()
            .any(|(line, _)| *line == 0)
    });
    draw(cx, window);
    click_gutter(cx, window, 0, 0);
    bounds_of(cx, window, "menu-0");
    click(cx, window, "menu-0");
    assert_eq!(active_path(cx, window), Some(repo));
}

#[ui_kit::gpui::test]
fn several_candidates_open_a_list_and_a_row_goes_to_its_place(cx: &mut TestAppContext) {
    let root = scratch("beans-list");
    let owners = write(&root, "Owners.txt", OWNERS);
    let repo = write(&root, "Repo.txt", REPO);
    let other = write(&root, "Other.txt", "class OtherRepo {}\n");
    let model = BeanModel {
        beans: vec![
            bean(
                "repo",
                "Repo",
                "@Component",
                &repo,
                0,
                6,
                vec![place("repo — Owners.txt", &owners, 21)],
            ),
            bean(
                "otherRepo",
                "OtherRepo",
                "@Component",
                &other,
                0,
                6,
                vec![place("otherRepo — Owners.txt", &owners, 21)],
            ),
        ],
        injections: vec![InjectionPoint {
            type_name: "Repo".to_owned(),
            qualified_type: "demo.Repo".to_owned(),
            file: owners.clone(),
            line: 1,
            offset: 21,
            range: 15..19,
            candidates: vec![place("repo", &repo, 6), place("otherRepo", &other, 6)],
        }],
        warnings: Vec::new(),
    };
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    open_path(cx, window, &owners);
    wait_for_signal(cx, "the badge of the point", |cx| {
        marks(cx, window, &owners)
            .iter()
            .any(|(line, _)| *line == 1)
    });
    draw(cx, window);
    click_gutter(cx, window, 0, 1);
    bounds_of(cx, window, "menu-0");
    bounds_of(cx, window, "menu-1");
    click(cx, window, "menu-1");
    assert_eq!(active_path(cx, window), Some(other));
}

#[ui_kit::gpui::test]
fn a_point_with_several_candidates_warns(cx: &mut TestAppContext) {
    let root = scratch("beans-warning");
    let owners = write(&root, "Owners.txt", OWNERS);
    let repo = write(&root, "Repo.txt", REPO);
    let message = "Could not autowire. There is more than one bean of 'Repo' type: otherRepo, repo";
    let model = BeanModel {
        beans: vec![bean("repo", "Repo", "@Component", &repo, 0, 6, Vec::new())],
        injections: vec![InjectionPoint {
            type_name: "Repo".to_owned(),
            qualified_type: "demo.Repo".to_owned(),
            file: owners.clone(),
            line: 1,
            offset: 21,
            range: 15..19,
            candidates: Vec::new(),
        }],
        warnings: vec![BeanWarning {
            file: owners.clone(),
            range: 15..19,
            id: "beans.noCandidate".to_owned(),
            message: message.to_owned(),
        }],
    };
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    open_path(cx, window, &owners);
    let view = editor_of(cx, window, &owners);
    wait_for_signal(cx, "the warning of the point", |cx| {
        view.read_with(cx, |view, _| {
            view.diagnostic_marks()
                .iter()
                .any(|(severity, _)| *severity == lang_api::Severity::Warning)
        })
    });
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    let messages: Vec<String> = cx.update(|cx| {
        problems
            .read(cx)
            .state()
            .of_file(&owners)
            .iter()
            .map(|problem| problem.message.clone())
            .collect()
    });
    assert!(
        messages.iter().any(|found| found == message),
        "{messages:?}"
    );
}

#[ui_kit::gpui::test]
fn the_window_lists_the_beans_by_module_and_stereotype_and_filters_them(cx: &mut TestAppContext) {
    let root = scratch("beans-window");
    let owners = write(&root, "Owners.txt", OWNERS);
    let repo = write(&root, "Repo.txt", REPO);
    let clock = write(&root, "Clock.txt", CLOCK);
    let model = BeanModel {
        beans: vec![
            bean("owners", "Owners", "@Component", &owners, 0, 6, Vec::new()),
            bean("repo", "Repo", "@Component", &repo, 0, 6, Vec::new()),
            bean("clock", "Clock", "@Bean", &clock, 3, 19, Vec::new()),
        ],
        injections: Vec::new(),
        warnings: Vec::new(),
    };
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(BEANS_TOOL, window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the beans of the window", |cx| !rows(cx).is_empty());
    let labels: Vec<String> = rows(cx).into_iter().map(|row| row.0).collect();
    assert!(labels.contains(&"demo".to_owned()), "{labels:?}");
    assert!(labels.contains(&"@Component".to_owned()), "{labels:?}");
    assert!(labels.contains(&"@Bean".to_owned()), "{labels:?}");
    assert!(labels.contains(&"clock".to_owned()), "{labels:?}");
    draw(cx, window);
    click(cx, window, "beans-filter");
    type_text(cx, window, "clock");
    wait_for_signal(cx, "the filtered beans", |cx| {
        rows(cx).iter().any(|row| row.0 == "clock")
    });
    let labels: Vec<String> = rows(cx).into_iter().map(|row| row.0).collect();
    assert!(labels.contains(&"clock".to_owned()), "{labels:?}");
    assert!(!labels.contains(&"owner".to_owned()), "{labels:?}");
    assert!(!labels.contains(&"repo".to_owned()), "{labels:?}");
}

/// The rows of the Beans window with the place of their bean.
fn rows(cx: &mut TestAppContext) -> Vec<(String, Option<usize>)> {
    cx.update(|cx| {
        cx.try_global::<ActiveBeans>()
            .map(|panel| {
                panel
                    .0
                    .read(cx)
                    .rows()
                    .iter()
                    .map(|row| (row.label.clone(), row.bean))
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[ui_kit::gpui::test]
fn a_click_on_a_bean_opens_its_declaration(cx: &mut TestAppContext) {
    let root = scratch("beans-open");
    let owners = write(&root, "Owners.txt", OWNERS);
    let repo = write(&root, "Repo.txt", REPO);
    let model = BeanModel {
        beans: vec![
            bean("owners", "Owners", "@Component", &owners, 0, 6, Vec::new()),
            bean("repo", "Repo", "@Component", &repo, 0, 6, Vec::new()),
        ],
        injections: Vec::new(),
        warnings: Vec::new(),
    };
    let window = open_window(cx, root.clone());
    let language = Arc::new(BeanLanguage::new(model));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
        })
        .unwrap();
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(BEANS_TOOL, window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the beans of the window", |cx| {
        rows(cx).iter().any(|row| row.1.is_some())
    });
    let row = cx.update(|cx| {
        cx.global::<ActiveBeans>()
            .0
            .read(cx)
            .rows()
            .iter()
            .position(|row| row.label == "repo")
            .unwrap()
    });
    window
        .update(cx, |_, window, cx| {
            let panel = cx.global::<ActiveBeans>().0.clone();
            panel.update(cx, |panel, cx| panel.open_row(row, window, cx));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(repo));
}

#[ui_kit::gpui::test]
fn the_hub_of_the_model_is_installed_with_the_window(cx: &mut TestAppContext) {
    let (window, _root) = open(
        cx,
        "beans-hub",
        &[("Owners.txt", OWNERS)],
        BeanModel::default(),
    );
    window
        .update(cx, |_, _, cx| {
            assert!(cx.try_global::<ActiveBeanHub>().is_some());
            assert!(cx.try_global::<Theme>().is_some());
        })
        .unwrap();
}
