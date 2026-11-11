//! Short interface animations.

use std::time::Instant;

use theme::motion;

/// A 0..1 value that eases towards its target; drives panel show and hide.
pub struct Slide {
    open: bool,
    from: f32,
    pub value: f32,
    started: Option<Instant>,
}

impl Slide {
    pub fn new(open: bool) -> Self {
        let value = if open { 1. } else { 0. };
        Self {
            open,
            from: value,
            value,
            started: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.from = self.value;
        self.started = Some(Instant::now());
    }

    /// Returns true while the animation still needs frames.
    pub fn step(&mut self) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        let t = (started.elapsed().as_secs_f32() * 1000. / motion::PANEL as f32).min(1.);
        let eased = 1. - (1. - t).powi(3);
        let target = if self.open { 1. } else { 0. };
        self.value = self.from + (target - self.from) * eased;
        if t >= 1. {
            self.started = None;
        }
        true
    }
}

pub fn smoothstep(t: f32) -> f32 {
    t * t * (3. - 2. * t)
}
