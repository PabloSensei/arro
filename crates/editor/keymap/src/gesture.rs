//! Double Shift and Double Ctrl. The modifier goes down, up, down and up again, alone and
//! quickly. It fires on the second release, so holding Ctrl after the second press stays
//! free for a gesture with another key.

use std::time::{Duration, Instant};

use crate::Modifier;

/// The longest step between two presses and releases. IntelliJ IDEA waits about as long.
pub const STEP: Duration = Duration::from_millis(300);

/// The modifiers held after a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    /// The one modifier held, `None` for none or several.
    fn single(self) -> Option<Modifier> {
        match (self.ctrl, self.alt, self.shift) {
            (true, false, false) => Some(Modifier::Ctrl),
            (false, true, false) => Some(Modifier::Alt),
            (false, false, true) => Some(Modifier::Shift),
            _ => None,
        }
    }

    fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Idle,
    Down(u8),
    Up(u8),
}

#[derive(Clone, Debug)]
pub struct GestureRecognizer {
    modifier: Option<Modifier>,
    step: Step,
    at: Instant,
}

impl Default for GestureRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl GestureRecognizer {
    pub fn new() -> Self {
        Self {
            modifier: None,
            step: Step::Idle,
            at: Instant::now(),
        }
    }

    /// Any other key in between breaks the gesture.
    pub fn key_down(&mut self) {
        self.step = Step::Idle;
    }

    /// Returns the modifier when this change completes a double press.
    pub fn modifiers_changed(&mut self, held: Modifiers, at: Instant) -> Option<Modifier> {
        let quick = at.saturating_duration_since(self.at) <= STEP;
        let single = held.single();
        let next = match self.step {
            Step::Up(1) if quick && single == self.modifier => Step::Down(2),
            Step::Down(count) if quick && held.is_empty() => Step::Up(count),
            _ => match single {
                Some(modifier) => {
                    self.modifier = Some(modifier);
                    Step::Down(1)
                }
                None => Step::Idle,
            },
        };
        self.at = at;
        if next == Step::Up(2) {
            self.step = Step::Idle;
            return self.modifier;
        }
        self.step = next;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: false,
    };
    const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
    };

    /// Feeds changes `gap` apart and collects what fired.
    fn play(gestures: &mut GestureRecognizer, changes: &[Modifiers], gap: u64) -> Vec<Modifier> {
        let start = Instant::now();
        changes
            .iter()
            .enumerate()
            .filter_map(|(index, held)| {
                let at = start + Duration::from_millis(gap * (index as u64 + 1));
                gestures.modifiers_changed(*held, at)
            })
            .collect()
    }

    #[test]
    fn a_quick_double_press_fires_on_the_second_release() {
        let mut gestures = GestureRecognizer::new();
        assert_eq!(
            play(&mut gestures, &[SHIFT, NONE, SHIFT, NONE], 80),
            [Modifier::Shift]
        );
        assert_eq!(
            play(&mut gestures, &[CTRL, NONE, CTRL, NONE], 80),
            [Modifier::Ctrl]
        );
    }

    #[test]
    fn slow_mixed_or_interrupted_presses_do_not() {
        let mut gestures = GestureRecognizer::new();
        assert!(play(&mut gestures, &[SHIFT, NONE, SHIFT, NONE], 400).is_empty());
        assert!(play(&mut gestures, &[SHIFT, NONE, CTRL, NONE], 80).is_empty());

        let start = Instant::now();
        let at = |step: u64| start + Duration::from_millis(50 * step);
        gestures.modifiers_changed(SHIFT, at(1));
        gestures.modifiers_changed(NONE, at(2));
        gestures.modifiers_changed(SHIFT, at(3));
        gestures.key_down();
        assert_eq!(gestures.modifiers_changed(NONE, at(4)), None);
    }

    #[test]
    fn a_slow_first_press_still_lets_a_quick_pair_follow() {
        let mut gestures = GestureRecognizer::new();
        let fired = play(&mut gestures, &[SHIFT, NONE, SHIFT, NONE, SHIFT, NONE], 80);
        assert_eq!(fired, [Modifier::Shift]);
    }
}
