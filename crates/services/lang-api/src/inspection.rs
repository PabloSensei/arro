//! The checks a language can run and the settings of the user for each of them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{DiagnosticEntry, Severity};

/// The level of a check, as the Settings window names it. `Off` turns the check off.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum InspectionLevel {
    Error,
    #[default]
    Warning,
    WeakWarning,
    Info,
    Off,
}

impl InspectionLevel {
    pub const ALL: [InspectionLevel; 5] = [
        InspectionLevel::Error,
        InspectionLevel::Warning,
        InspectionLevel::WeakWarning,
        InspectionLevel::Info,
        InspectionLevel::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InspectionLevel::Error => "Error",
            InspectionLevel::Warning => "Warning",
            InspectionLevel::WeakWarning => "Weak Warning",
            InspectionLevel::Info => "Info",
            InspectionLevel::Off => "Off",
        }
    }

    /// The severity the editor paints, or `None` for a check that is off.
    pub fn severity(self) -> Option<Severity> {
        match self {
            InspectionLevel::Error => Some(Severity::Error),
            InspectionLevel::Warning => Some(Severity::Warning),
            InspectionLevel::WeakWarning => Some(Severity::Hint),
            InspectionLevel::Info => Some(Severity::Information),
            InspectionLevel::Off => None,
        }
    }

    pub fn from_severity(severity: Severity) -> Self {
        match severity {
            Severity::Error => InspectionLevel::Error,
            Severity::Warning => InspectionLevel::Warning,
            Severity::Information => InspectionLevel::Info,
            Severity::Hint => InspectionLevel::WeakWarning,
        }
    }

    pub fn next(self) -> Self {
        let at = Self::ALL
            .iter()
            .position(|level| *level == self)
            .unwrap_or(1);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

/// One check of a language, as Settings lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionInfo {
    /// The stable name the settings and the suppression comments use.
    pub id: String,
    pub title: String,
    /// The group of the tree the check belongs to.
    pub group: String,
    pub default_severity: InspectionLevel,
    /// Whether the check can be turned off; a check the language only reports cannot.
    pub configurable: bool,
}

/// What the user chose for one check. A missing level keeps the default of the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectionSetting {
    pub enabled: bool,
    pub level: Option<InspectionLevel>,
}

impl Default for InspectionSetting {
    fn default() -> Self {
        Self {
            enabled: true,
            level: None,
        }
    }
}

/// The choices of the user for the checks of one project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectionSettings {
    #[serde(flatten)]
    pub inspections: BTreeMap<String, InspectionSetting>,
}

impl InspectionSettings {
    pub fn setting(&self, id: &str) -> Option<InspectionSetting> {
        self.inspections.get(id).copied()
    }

    /// The level in force: the choice of the user, or the default of the check.
    pub fn level_of(&self, id: &str, default: InspectionLevel) -> InspectionLevel {
        match self.setting(id) {
            Some(setting) if !setting.enabled => InspectionLevel::Off,
            Some(setting) => setting.level.unwrap_or(default),
            None => default,
        }
    }

    pub fn set(&mut self, id: &str, enabled: bool, level: Option<InspectionLevel>) {
        self.inspections.insert(
            id.to_owned(),
            InspectionSetting {
                enabled,
                level: if enabled { level } else { None },
            },
        );
    }

    /// Removes every choice, so the defaults of the checks come back.
    pub fn reset(&mut self) {
        self.inspections.clear();
    }

    /// The entries with the levels of the user: a check that is off does not show, and a
    /// check that names another level replaces the severity of the language. An entry
    /// without a code, or with a code the user did not name, stays as it is.
    pub fn apply(&self, entries: &mut Vec<DiagnosticEntry>) {
        entries.retain_mut(|entry| {
            let Some(code) = entry.code.as_deref() else {
                return true;
            };
            let Some(setting) = self.setting(code) else {
                return true;
            };
            if !setting.enabled {
                return false;
            }
            if let Some(level) = setting.level
                && let Some(severity) = level.severity()
            {
                entry.severity = severity;
            }
            true
        });
    }

    /// The same as [`Self::apply`], with the `//noinspection` comments of the text.
    pub fn apply_with_suppressions(
        &self,
        entries: &mut Vec<DiagnosticEntry>,
        suppressions: &Suppressions,
    ) {
        entries.retain_mut(|entry| {
            let Some(code) = entry.code.as_deref() else {
                return true;
            };
            if suppressions.covers(code, entry.span.start.line) {
                return false;
            }
            let Some(setting) = self.setting(code) else {
                return true;
            };
            if !setting.enabled {
                return false;
            }
            if let Some(level) = setting.level
                && let Some(severity) = level.severity()
            {
                entry.severity = severity;
            }
            true
        });
    }
}

/// The checks a language does not compute at all. Cloning shares the state, so the
/// handle the language holds reaches every provider it gave the handle to. An empty
/// set, the state before the project settings arrive, keeps every check on.
#[derive(Clone, Debug, Default)]
pub struct DisabledInspections {
    ids: Arc<Mutex<HashSet<String>>>,
}

impl DisabledInspections {
    /// Takes the checks that are off from `settings`. A check the user turned off is
    /// skipped by its provider; the level of the others changes only what the editor
    /// paints, so the providers ignore it.
    pub fn replace(&self, settings: &InspectionSettings) {
        let mut ids = self.ids.lock().unwrap_or_else(|poison| poison.into_inner());
        ids.clear();
        ids.extend(
            settings
                .inspections
                .iter()
                .filter(|(_, setting)| !setting.enabled)
                .map(|(id, _)| id.clone()),
        );
    }

    pub fn enabled(&self, id: &str) -> bool {
        !self
            .ids
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(id)
    }

    /// Whether at least one of `ids` is on: a check that is off alone costs no work.
    pub fn any_enabled(&self, ids: &[&str]) -> bool {
        ids.iter().any(|id| self.enabled(id))
    }
}

/// The checks suppressed by the `//noinspection` comments of one text, by line. The
/// comment suppresses the line after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suppressions {
    by_line: HashMap<u32, Vec<String>>,
}

impl Suppressions {
    pub fn from_text(text: &str) -> Self {
        let mut by_line: HashMap<u32, Vec<String>> = HashMap::new();
        for (index, line) in text.lines().enumerate() {
            let Some(ids) = comment_ids(line) else {
                continue;
            };
            by_line.entry(index as u32 + 1).or_default().extend(ids);
        }
        Self { by_line }
    }

    pub fn covers(&self, id: &str, line: u32) -> bool {
        let Some(ids) = self.by_line.get(&line) else {
            return false;
        };
        ids.iter()
            .any(|suppressed| suppressed == id || suppressed == "all")
    }
}

/// The ids of a `//noinspection id, id` comment, or `None` for another line.
fn comment_ids(line: &str) -> Option<Vec<String>> {
    let text = line.trim_start();
    let rest = text.strip_prefix("//noinspection")?;
    Some(
        rest.split([',', ' '])
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiagnosticEntry, TextPoint, TextSpan};

    fn entry(line: u32, code: &str, severity: Severity) -> DiagnosticEntry {
        DiagnosticEntry {
            span: TextSpan {
                start: TextPoint { line, character: 0 },
                end: TextPoint { line, character: 4 },
            },
            severity,
            message: "problem".to_owned(),
            unnecessary: false,
            code: Some(code.to_owned()),
        }
    }

    #[test]
    fn the_levels_walk_the_settings_window_order() {
        assert_eq!(InspectionLevel::Error.next(), InspectionLevel::Warning);
        assert_eq!(InspectionLevel::Info.next(), InspectionLevel::Off);
        assert_eq!(InspectionLevel::Off.next(), InspectionLevel::Error);
        assert_eq!(
            InspectionLevel::WeakWarning.severity(),
            Some(Severity::Hint)
        );
        assert_eq!(InspectionLevel::Off.severity(), None);
    }

    #[test]
    fn the_settings_turn_a_check_off_and_change_the_level_of_another() {
        let mut settings = InspectionSettings::default();
        settings.set("a.off", false, None);
        settings.set("a.error", true, Some(InspectionLevel::Error));
        let mut entries = vec![
            entry(1, "a.off", Severity::Warning),
            entry(2, "a.error", Severity::Warning),
            entry(3, "a.other", Severity::Warning),
        ];

        settings.apply(&mut entries);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].code.as_deref(), Some("a.error"));
        assert_eq!(entries[0].severity, Severity::Error);
        assert_eq!(entries[1].severity, Severity::Warning);
    }

    #[test]
    fn a_disabled_check_reaches_every_handle_of_the_language() {
        let mut settings = InspectionSettings::default();
        settings.set("a.off", false, None);
        settings.set("a.error", true, Some(InspectionLevel::Error));
        let disabled = DisabledInspections::default();
        assert!(
            disabled.enabled("a.off"),
            "nothing is off before the settings"
        );

        let provider = disabled.clone();
        disabled.replace(&settings);

        assert!(!provider.enabled("a.off"));
        assert!(provider.enabled("a.error"));
        assert!(provider.enabled("a.unknown"));
        assert!(provider.any_enabled(&["a.off", "a.error"]));
        assert!(!provider.any_enabled(&["a.off"]));

        disabled.replace(&InspectionSettings::default());
        assert!(provider.enabled("a.off"), "a reset turns every check on");
    }

    #[test]
    fn a_comment_suppresses_the_line_below_and_only_the_named_check() {
        let text = "//noinspection a.one\nlet x = 1;\n//noinspection a.two, a.three\nlet y = 2;\n";
        let suppressions = Suppressions::from_text(text);
        assert!(suppressions.covers("a.one", 1));
        assert!(!suppressions.covers("a.two", 1));
        assert!(suppressions.covers("a.two", 3));
        assert!(suppressions.covers("a.three", 3));
        assert!(!suppressions.covers("a.one", 3));

        let mut entries = vec![
            entry(1, "a.one", Severity::Warning),
            entry(1, "a.two", Severity::Warning),
        ];
        let settings = InspectionSettings::default();
        settings.apply_with_suppressions(&mut entries, &suppressions);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].code.as_deref(), Some("a.two"));
    }

    #[test]
    fn a_comment_with_all_suppresses_every_check_of_the_line() {
        let suppressions = Suppressions::from_text("//noinspection all\nlet x = 1;\n");
        assert!(suppressions.covers("a.one", 1));
        assert!(suppressions.covers("a.three", 1));
    }
}
