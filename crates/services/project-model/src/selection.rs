//! The answers the user gives to the build of a project: which of its named settings are
//! on, and the named values it reads. A value only travels to the build tool, so nothing
//! here knows which tool the project uses.

use serde::{Deserialize, Serialize};

use crate::BuildProfile;

/// One named value the build reads, as `name=value`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildProperty {
    pub name: String,
    pub value: String,
}

/// What the user chose for one named setting of the build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileChoice {
    /// The build file decides.
    #[default]
    Default,
    /// Turned on for the build.
    On,
    /// Turned off for the build.
    Off,
}

/// The answers of the user for the build of a project. A missing or damaged file of these
/// answers is [`BuildSelection::default`]: the build file decides alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildSelection {
    /// The named settings turned on although the build file does not turn them on.
    pub enabled: Vec<String>,
    /// The named settings turned off although the build file turns them on.
    pub disabled: Vec<String>,
    /// The named values the build reads.
    pub properties: Vec<BuildProperty>,
}

impl BuildSelection {
    /// The answer for one named setting.
    pub fn choice(&self, name: &str) -> ProfileChoice {
        if self.enabled.iter().any(|known| known == name) {
            ProfileChoice::On
        } else if self.disabled.iter().any(|known| known == name) {
            ProfileChoice::Off
        } else {
            ProfileChoice::Default
        }
    }

    /// Answers for one named setting, dropping the other answer.
    pub fn set_choice(&mut self, name: &str, choice: ProfileChoice) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.enabled.retain(|known| known != name);
        self.disabled.retain(|known| known != name);
        match choice {
            ProfileChoice::On => self.enabled.push(name.to_owned()),
            ProfileChoice::Off => self.disabled.push(name.to_owned()),
            ProfileChoice::Default => {}
        }
    }

    /// The next answer of a check box: the build file, then on, then off, then the build
    /// file again.
    pub fn cycle(&mut self, name: &str) -> ProfileChoice {
        let next = match self.choice(name) {
            ProfileChoice::Default => ProfileChoice::On,
            ProfileChoice::On => ProfileChoice::Off,
            ProfileChoice::Off => ProfileChoice::Default,
        };
        self.set_choice(name, next);
        next
    }

    /// Whether the build turns the setting on: the answer of the user wins over the one of
    /// the build file.
    pub fn active(&self, profile: &BuildProfile) -> bool {
        match self.choice(&profile.name) {
            ProfileChoice::On => true,
            ProfileChoice::Off => false,
            ProfileChoice::Default => profile.active_by_default,
        }
    }

    /// The value of one named setting.
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| property.value.as_str())
    }

    /// The values as the window edits them: `name=value; name=value`.
    pub fn properties_text(&self) -> String {
        let pairs = self
            .properties
            .iter()
            .map(|property| format!("{}={}", property.name, property.value));
        pairs.collect::<Vec<_>>().join("; ")
    }

    /// Replaces the values with what the window holds. A piece without `=` is not a value
    /// and is left out; the last of two values of one name wins.
    pub fn set_properties_text(&mut self, text: &str) {
        self.properties = parse_properties(text);
    }
}

fn parse_properties(text: &str) -> Vec<BuildProperty> {
    let mut properties: Vec<BuildProperty> = Vec::new();
    for piece in text.split(';') {
        let Some((name, value)) = piece.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let value = value.trim().to_owned();
        match properties.iter_mut().find(|property| property.name == name) {
            Some(found) => found.value = value,
            None => properties.push(BuildProperty {
                name: name.to_owned(),
                value,
            }),
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, active_by_default: bool) -> BuildProfile {
        BuildProfile {
            name: name.into(),
            active_by_default,
            ..BuildProfile::default()
        }
    }

    #[test]
    fn a_setting_cycles_from_the_file_to_on_to_off_and_back() {
        let mut selection = BuildSelection::default();
        let plain = profile("dev", false);

        assert_eq!(selection.choice("dev"), ProfileChoice::Default);
        assert!(!selection.active(&plain), "the file leaves it off");

        assert_eq!(selection.cycle("dev"), ProfileChoice::On);
        assert!(selection.active(&plain), "the user turned it on");
        assert_eq!(selection.enabled, ["dev"]);
        assert!(selection.disabled.is_empty());

        assert_eq!(selection.cycle("dev"), ProfileChoice::Off);
        assert!(!selection.active(&plain));
        assert_eq!(selection.disabled, ["dev"]);
        assert!(selection.enabled.is_empty());

        assert_eq!(selection.cycle("dev"), ProfileChoice::Default);
        assert_eq!(selection.choice("dev"), ProfileChoice::Default);
        assert!(selection.enabled.is_empty());
        assert!(selection.disabled.is_empty());
    }

    #[test]
    fn the_answer_of_the_user_wins_over_the_file() {
        let turned_on = profile("default-on", true);
        let mut selection = BuildSelection::default();
        assert!(
            selection.active(&turned_on),
            "the file turns it on by itself"
        );

        selection.set_choice("default-on", ProfileChoice::Off);
        assert!(!selection.active(&turned_on));
        assert_eq!(selection.choice("default-on"), ProfileChoice::Off);
    }

    #[test]
    fn the_values_are_edited_as_one_line() {
        let mut selection = BuildSelection::default();
        selection.set_properties_text("parallel=true; workers = 4; broken;  = 1");

        assert_eq!(
            selection.properties,
            [
                BuildProperty {
                    name: "parallel".into(),
                    value: "true".into(),
                },
                BuildProperty {
                    name: "workers".into(),
                    value: "4".into(),
                },
            ],
            "a piece without a name or without = is left out"
        );
        assert_eq!(selection.property("workers"), Some("4"));
        assert_eq!(selection.properties_text(), "parallel=true; workers=4");
    }

    #[test]
    fn the_last_of_two_values_of_one_name_wins() {
        let mut selection = BuildSelection::default();
        selection.set_properties_text("mode=one; mode=two");

        assert_eq!(selection.properties_text(), "mode=two");
    }

    #[test]
    fn the_answers_survive_a_round_trip() {
        let mut selection = BuildSelection::default();
        selection.set_choice("dev", ProfileChoice::On);
        selection.set_choice("prod", ProfileChoice::Off);
        selection.set_properties_text("mode=fast");

        let text = serde_json::to_string(&selection).unwrap();
        let read: BuildSelection = serde_json::from_str(&text).unwrap();

        assert_eq!(read, selection);
    }

    /// A file written before the properties were added names no values and must still load.
    #[test]
    fn an_older_file_still_loads() {
        let read: BuildSelection =
            serde_json::from_str(r#"{"enabled":["dev"],"disabled":["prod"]}"#).unwrap();

        assert_eq!(read.enabled, ["dev"]);
        assert_eq!(read.disabled, ["prod"]);
        assert!(read.properties.is_empty());
    }
}
