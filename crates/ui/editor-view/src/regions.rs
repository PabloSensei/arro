//! Go to Custom Region (Ctrl+Alt+.): the named regions of the file and a filter over
//! their names.

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::view::EditorView;

pub(crate) struct RegionPopup {
    pub query: TextField,
    pub selected: usize,
}

impl EditorView {
    /// Ctrl+Alt+.: opens the list of regions at the caret.
    pub(crate) fn open_region_popup(&mut self, cx: &mut Context<Self>) {
        if self.state.is_none() || self.regions_empty() {
            return;
        }
        self.region_popup = Some(RegionPopup {
            query: TextField::default(),
            selected: 0,
        });
        cx.notify();
    }

    fn regions_empty(&self) -> bool {
        self.state
            .as_ref()
            .is_none_or(|state| state.regions().is_empty())
    }

    /// Keys while the region list is open. The list takes every key, so nothing of it
    /// reaches the text below.
    pub(crate) fn region_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.region_popup.is_none() {
            return false;
        }
        let key = event.keystroke.key.as_str();
        let query = self
            .region_popup
            .as_ref()
            .map(|popup| popup.query.text.clone())
            .unwrap_or_default();
        match key {
            "escape" => {
                self.region_popup = None;
                cx.notify();
            }
            "enter" => {
                let found = self.regions_matching(&query);
                let selected = self.region_popup.as_ref().map_or(0, |popup| popup.selected);
                let line = found.get(selected).map(|region| region.line);
                self.region_popup = None;
                if let Some(line) = line {
                    self.go_to_line(line, cx);
                } else {
                    cx.notify();
                }
            }
            "down" => {
                let found = self.regions_matching(&query).len();
                if let Some(popup) = &mut self.region_popup
                    && found > 0
                {
                    popup.selected = (popup.selected + 1) % found;
                }
                cx.notify();
            }
            "up" => {
                let found = self.regions_matching(&query).len();
                if let Some(popup) = &mut self.region_popup
                    && found > 0
                {
                    popup.selected = (popup.selected + found - 1) % found;
                }
                cx.notify();
            }
            _ => {
                if let Some(popup) = &mut self.region_popup
                    && popup.query.key_with_clipboard(event, false, cx)
                {
                    popup.selected = 0;
                    cx.notify();
                }
            }
        }
        true
    }

    /// The regions whose name holds the query, as typed.
    fn regions_matching(&self, query: &str) -> Vec<syntax::Region> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let query = query.to_ascii_lowercase();
        state
            .regions()
            .into_iter()
            .filter(|region| region.name.to_ascii_lowercase().contains(&query))
            .collect()
    }

    pub(crate) fn render_region_popup(
        &self,
        popup: &RegionPopup,
        theme: &Theme,
    ) -> Vec<AnyElement> {
        let found = self.regions_matching(&popup.query.text);
        let height =
            (found.len().min(crate::semantic::MAX_SHOWN) + 2) as f32 * self.style.line_height + 8.;
        let at = self.head_offset().unwrap_or(0);
        let Some((position, anchor)) = self.popup_position(at, height) else {
            return Vec::new();
        };
        let rows = found
            .iter()
            .take(crate::semantic::MAX_SHOWN)
            .enumerate()
            .map(|(row, region)| {
                let selected = row == popup.selected;
                div()
                    .id(("region-row", row))
                    .debug_selector(move || format!("region-row-{row}"))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .when(selected, |row| row.bg(theme.selection))
                    .text_color(theme.text)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(region.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(format!(":{}", region.line + 1)),
                    )
            });
        let body = surface(theme)
            .debug_selector(|| "goto-region-popup".to_owned())
            .min_w(px(280.))
            .max_w(px(640.))
            .py_1()
            .font_family(self.style.font_family.clone())
            .text_size(px(self.style.font_size))
            .child(
                div()
                    .px_2()
                    .flex()
                    .items_center()
                    .text_color(theme.text_muted)
                    .child("Region ")
                    .children(text_field_children_named(
                        "region-query-field",
                        &popup.query,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        self.style.line_height,
                        true,
                    )),
            )
            .children(rows)
            .when(found.is_empty(), |body| {
                body.child(
                    div()
                        .px_2()
                        .text_color(theme.text_muted)
                        .child("No matching region"),
                )
            });
        vec![
            deferred(
                anchored()
                    .anchor(anchor)
                    .position(position)
                    .snap_to_window_with_margin(px(SPACE_M))
                    .child(body),
            )
            .with_priority(1)
            .into_any_element(),
        ]
    }
}
