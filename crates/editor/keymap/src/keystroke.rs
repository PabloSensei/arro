use std::fmt;

use crate::KeymapError;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Keystroke {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Lower case key name: `a`, `/`, `enter`, `up`, `numpad_add`.
    pub key: String,
}

impl Keystroke {
    /// Parses `ctrl-shift-z`. The last part is the key, so `ctrl--` is Ctrl and minus.
    pub fn parse(text: &str) -> Result<Self, KeymapError> {
        let invalid = || KeymapError::InvalidKeystroke(text.to_owned());
        let mut stroke = Self::default();
        let mut rest = text;
        while let Some((modifier, tail)) = rest.split_once('-') {
            if tail.is_empty() {
                break;
            }
            match modifier {
                "ctrl" => stroke.ctrl = true,
                "alt" => stroke.alt = true,
                "shift" => stroke.shift = true,
                _ => return Err(invalid()),
            }
            rest = tail;
        }
        if rest.is_empty() {
            return Err(invalid());
        }
        stroke.key = rest.to_lowercase();
        Ok(stroke)
    }

    /// The same physical key as the US layout names it. A letter typed on the Russian
    /// layout becomes the Latin letter of its key, so `ctrl-ш` is `ctrl-i`. A shifted
    /// character comes back as its key and Shift, because the platform folds Shift into
    /// the character on layouts with another script: `ctrl-?` is `ctrl-shift-/`.
    pub fn normalized(mut self) -> Self {
        let mut characters = self.key.chars();
        if let (Some(character), None) = (characters.next(), characters.next()) {
            if let Some(latin) = russian_to_us(character) {
                self.key = latin.to_string();
            } else if let Some(key) = us_key_of_shifted(character) {
                self.key = key.to_string();
                self.shift = true;
            }
        }
        self
    }
}

/// The ЙЦУКЕН letters on the keys of QWERTY.
const RUSSIAN: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
const US: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";

fn russian_to_us(character: char) -> Option<char> {
    let lower = character.to_lowercase().next()?;
    let index = RUSSIAN.chars().position(|letter| letter == lower)?;
    US.chars().nth(index)
}

/// The key that types a shifted character on the US layout. Only characters that no
/// unshifted key types are here, so the key of every one of them is known.
fn us_key_of_shifted(character: char) -> Option<char> {
    Some(match character {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}

impl fmt::Display for Keystroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key)
    }
}

/// A modifier pressed twice alone: Double Shift, Double Ctrl.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
}

/// What a binding is pressed with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Shortcut {
    /// One keystroke, or two for a chord like `ctrl-k ctrl-c`.
    Keys(Vec<Keystroke>),
    Double(Modifier),
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Self, KeymapError> {
        let double = match text {
            "double-ctrl" => Some(Modifier::Ctrl),
            "double-alt" => Some(Modifier::Alt),
            "double-shift" => Some(Modifier::Shift),
            _ => None,
        };
        if let Some(modifier) = double {
            return Ok(Self::Double(modifier));
        }
        let keys: Vec<Keystroke> = text
            .split(' ')
            .map(Keystroke::parse)
            .collect::<Result<_, _>>()?;
        if keys.len() > 2 {
            return Err(KeymapError::InvalidKeystroke(text.to_owned()));
        }
        Ok(Self::Keys(keys))
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keys(keys) => {
                for (index, key) in keys.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{key}")?;
                }
                Ok(())
            }
            Self::Double(Modifier::Ctrl) => f.write_str("double-ctrl"),
            Self::Double(Modifier::Alt) => f.write_str("double-alt"),
            Self::Double(Modifier::Shift) => f.write_str("double-shift"),
        }
    }
}
