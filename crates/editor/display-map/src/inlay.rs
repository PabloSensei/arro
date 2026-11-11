//! Text shown between characters that is not part of the buffer.

use text_core::Edit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inlay {
    pub offset: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct InlayMap {
    inlays: Vec<Inlay>,
}

impl InlayMap {
    pub fn as_slice(&self) -> &[Inlay] {
        &self.inlays
    }

    pub fn set(&mut self, mut inlays: Vec<Inlay>) {
        inlays.sort_by_key(|inlay| inlay.offset);
        self.inlays = inlays;
    }

    /// Inlays inside a replaced range go away, later ones move.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            let delta = edit.text.len() as isize - edit.range.len() as isize;
            self.inlays
                .retain(|inlay| inlay.offset <= edit.range.start || inlay.offset >= edit.range.end);
            for inlay in &mut self.inlays {
                if inlay.offset >= edit.range.end && inlay.offset > edit.range.start {
                    inlay.offset = inlay.offset.checked_add_signed(delta).unwrap_or(0);
                }
            }
        }
    }
}
