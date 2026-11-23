//! The Scope of the hierarchy windows: the folders of the project that a window shows rows
//! from. The Type Hierarchy and the Call Hierarchy share it, with its three buttons and the
//! Expand All and Collapse All buttons beside them, so that both windows keep the same folders.

use std::path::PathBuf;

use project_model::TEST_UNIT;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::script::ActiveRunPanel;

/// Which folders of the project a hierarchy window shows rows from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyScope {
    /// Every row, wherever it lives.
    All,
    /// The rows of the source folders, without the tests.
    Production,
    /// The rows of the test folders only.
    Test,
}

impl HierarchyScope {
    fn title(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Production => "Production",
            Self::Test => "Test",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::All => "Show the rows of the whole project",
            Self::Production => "Show the rows of the source folders",
            Self::Test => "Show the rows of the test folders",
        }
    }

    /// The end of the id of the button, after the name of the window.
    fn slug(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Production => "production",
            Self::Test => "test",
        }
    }
}

/// The folders that make a row a source row or a test row, from the project model. The
/// folders are compared as [`platform::path_key`] does, so the case and the separators of a
/// model path and a server URI do not matter.
#[derive(Clone, Debug, Default)]
pub struct ScopeRoots {
    source: Vec<PathBuf>,
    test: Vec<PathBuf>,
}

impl ScopeRoots {
    /// Reads the source and test folders of the project from the model of the run window.
    /// Without a model no folder is known to be a test one, so the scope keeps every row.
    pub fn from_model(cx: &App) -> Self {
        let model = cx
            .try_global::<ActiveRunPanel>()
            .map(|active| active.0.clone())
            .and_then(|runs| runs.read(cx).model());
        let Some(model) = model else {
            return Self::default();
        };
        let mut found = Self::default();
        for module in &model.modules {
            for unit in &module.units {
                let folders = if unit.name == TEST_UNIT {
                    &mut found.test
                } else {
                    &mut found.source
                };
                folders.extend(
                    unit.source_roots
                        .iter()
                        .map(|root| ::platform::path_key(root)),
                );
            }
        }
        found.source.sort();
        found.source.dedup();
        found.test.sort();
        found.test.dedup();
        found
    }

    /// Whether `scope` keeps a row whose place is `uri`. A row outside the project, such as one
    /// of the JDK or of a library, has no folder of the model and stays in every scope.
    pub fn keeps(&self, scope: HierarchyScope, uri: Option<&str>) -> bool {
        if scope == HierarchyScope::All || (self.source.is_empty() && self.test.is_empty()) {
            return true;
        }
        let Some(path) = uri.and_then(editor_view::uri_path) else {
            return true;
        };
        let path = ::platform::path_key(&path);
        let in_test = self.test.iter().any(|root| path.starts_with(root));
        let in_source = !in_test && self.source.iter().any(|root| path.starts_with(root));
        match scope {
            HierarchyScope::All => true,
            HierarchyScope::Production => !in_test,
            HierarchyScope::Test => !in_source,
        }
    }
}

/// The Scope of a hierarchy window with its three buttons, then Expand All and Collapse All:
/// the controls of the right part of the header. `window` names the window in the ids of the
/// controls, so the Call Hierarchy has `call-hierarchy-scope-test` where the Type Hierarchy has
/// `type-hierarchy-scope-test`.
pub fn header_controls<V: 'static>(
    window: &'static str,
    scope: HierarchyScope,
    theme: &Theme,
    cx: &mut Context<V>,
    set_scope: fn(&mut V, HierarchyScope, &mut Context<V>),
    expand_all: fn(&mut V, &mut Context<V>),
    collapse_all: fn(&mut V, &mut Context<V>),
) -> Vec<AnyElement> {
    let mut controls = vec![
        div()
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_secondary)
            .child("Scope")
            .into_any_element(),
    ];
    for kind in [
        HierarchyScope::All,
        HierarchyScope::Production,
        HierarchyScope::Test,
    ] {
        controls.push(
            scope_button(window, kind, scope == kind, theme)
                .on_click(cx.listener(move |this, _, _, cx| set_scope(this, kind, cx)))
                .into_any_element(),
        );
    }
    controls.push(
        icon_button(
            format!("{window}-expand-all"),
            "general/expandAll",
            "Expand All",
            Some("Ctrl+Equal".into()),
            theme,
        )
        .debug_selector(move || format!("{window}-expand-all"))
        .on_click(cx.listener(move |this, _, _, cx| expand_all(this, cx)))
        .into_any_element(),
    );
    controls.push(
        icon_button(
            format!("{window}-collapse-all"),
            "general/collapseAll",
            "Collapse All",
            Some("Ctrl+Minus".into()),
            theme,
        )
        .debug_selector(move || format!("{window}-collapse-all"))
        .on_click(cx.listener(move |this, _, _, cx| collapse_all(this, cx)))
        .into_any_element(),
    );
    controls
}

/// One button of the Scope, marked while it is the scope in force.
fn scope_button(
    window: &'static str,
    scope: HierarchyScope,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id = format!("{window}-scope-{}", scope.slug());
    let selector = id.clone();
    let text = if active {
        theme.accent
    } else {
        theme.text_secondary
    };
    div()
        .id(id)
        .debug_selector(move || selector)
        .px(px(SPACE_S))
        .py(px(SPACE_XS))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .text_color(text)
        .when(active, |button| button.bg(theme.hover))
        .tooltip(move |_, cx| tooltip(scope.hint(), None, cx))
        .child(scope.title())
}
