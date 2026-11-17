//! The choice between the text of a `.properties` file and the table of the localized set
//! it belongs to. The table itself comes from the application: `workspace` knows the
//! tabs, the editor only asks for the view of a set and shows it in place of the text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::view::EditorView;

/// What a tab of a file of a localized set shows: the text or the table of the set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BundleMode {
    #[default]
    Text,
    Bundle,
}

/// The mode picked for each file of the session, so a tab opened again shows what it
/// showed when it closed. The saved layout keeps the same choice for a project.
#[derive(Default)]
pub struct BundleModes(HashMap<PathBuf, BundleMode>);

impl Global for BundleModes {}

/// Whether a file belongs to a set the application can show.
pub type BundleAvailable = dyn Fn(&Path, &App) -> bool;
/// The table of a set, built by the application for the tab of one of its files.
pub type BundleBuilder = dyn Fn(&Path, &mut Window, &mut App) -> Option<AnyElement>;
/// Told when the mode of a tab changes, so the application keeps it in the layout.
pub type BundleChanged = dyn Fn(&mut App);

/// What the application gives the editor of a file of a localized set: whether the file
/// has a table, the table itself and a note that the mode changed.
pub struct BundleHost {
    pub available: Rc<BundleAvailable>,
    pub build: Rc<BundleBuilder>,
    pub changed: Rc<BundleChanged>,
}

impl Global for BundleHost {}

impl EditorView {
    /// The mode of this tab, `None` for a file that has no table of a set.
    pub fn bundle_mode(&self, cx: &App) -> Option<BundleMode> {
        if !self.bundle_available(cx) {
            return None;
        }
        Some(
            cx.try_global::<BundleModes>()
                .and_then(|modes| modes.0.get(&self.path).copied())
                .unwrap_or_default(),
        )
    }

    /// Shows the text or the table of the set, and keeps the choice for the file.
    pub fn set_bundle_mode(&mut self, mode: BundleMode, cx: &mut Context<Self>) {
        if !self.bundle_available(cx) || self.bundle_mode(cx) == Some(mode) {
            return;
        }
        cx.default_global::<BundleModes>()
            .0
            .insert(self.path.clone(), mode);
        if let Some(host) = cx.try_global::<BundleHost>() {
            let changed = host.changed.clone();
            changed(cx);
        }
        cx.notify();
    }

    fn bundle_available(&self, cx: &App) -> bool {
        cx.try_global::<BundleHost>()
            .is_some_and(|host| (host.available)(&self.path, cx))
    }

    /// The two modes of a file of a localized set, in the top right corner of the tab.
    pub(crate) fn bundle_toolbar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let available = self.bundle_available(cx);
        if !available {
            return None;
        }
        let mode = self.bundle_mode(cx)?;
        let theme = *cx.global::<Theme>();
        let button = |id: &'static str, icon: &str, label: &'static str, active: bool| {
            let button = icon_button(id, icon, label, None, &theme)
                .relative()
                .debug_selector(move || id.to_owned());
            if active {
                button.bg(theme.selection)
            } else {
                button
            }
        };
        let text = button(
            "bundle-mode-text",
            "fileTypes/text",
            "Text",
            mode == BundleMode::Text,
        )
        .on_click(cx.listener(|this, _, _, cx| this.set_bundle_mode(BundleMode::Text, cx)));
        let bundle = button(
            "bundle-mode-bundle",
            "toolwindows/structure",
            "Resource Bundle",
            mode == BundleMode::Bundle,
        )
        .on_click(cx.listener(|this, _, _, cx| this.set_bundle_mode(BundleMode::Bundle, cx)));
        Some(
            div()
                .flex()
                .items_center()
                .justify_end()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .pr(px(SPACE_S))
                .gap(px(SPACE_XS))
                .child(text)
                .child(bundle),
        )
    }

    /// The table of the set, when the tab shows it instead of the text.
    pub(crate) fn bundle_area(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.bundle_mode(cx) != Some(BundleMode::Bundle) {
            return None;
        }
        let host = cx.try_global::<BundleHost>()?;
        let build = host.build.clone();
        build(&self.path, window, cx)
    }
}
