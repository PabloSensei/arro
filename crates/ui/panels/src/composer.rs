//! The message box of a chat: text with `@` mentions of project files and of the editor
//! selection. The logic lives here without GPUI; the panel draws it.

use std::path::{Path, PathBuf};

use agent_core::PromptPart;

/// The mention of whatever the editor has selected when the message goes out.
pub const SELECTION: &str = "selection";
const SHOWN: usize = 8;

/// What the editor had selected: its file, first and last line from 1, and the text.
pub type Selection = (PathBuf, (u32, u32), String);

#[derive(Default)]
pub struct Composer {
    pub text: String,
    /// The highlighted row of the mention list.
    pub chosen: usize,
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

impl Composer {
    pub fn insert(&mut self, text: &str) {
        self.text.push_str(text);
        self.chosen = 0;
    }

    pub fn backspace(&mut self) {
        self.text.pop();
        self.chosen = 0;
    }

    pub fn take(&mut self) -> String {
        self.chosen = 0;
        std::mem::take(&mut self.text)
    }

    /// The word being typed after `@`, if the caret is in one.
    pub fn mention_query(&self) -> Option<&str> {
        let start = self.text.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        self.text[start..].strip_prefix('@')
    }

    /// Candidates for the mention being typed: the selection first, then matching files.
    pub fn candidates(&self, root: &Path, files: &[PathBuf]) -> Vec<String> {
        let Some(query) = self.mention_query() else {
            return Vec::new();
        };
        let query = query.to_lowercase();
        let mut found = Vec::new();
        if SELECTION.starts_with(&query) {
            found.push(SELECTION.to_string());
        }
        let names = files.iter().map(|file| relative(root, file));
        let matching = names.filter(|name| name.to_lowercase().contains(&query));
        found.extend(matching.take(SHOWN));
        found
    }

    pub fn move_choice(&mut self, delta: isize, count: usize) {
        if count == 0 {
            return;
        }
        let next = self.chosen as isize + delta;
        self.chosen = next.rem_euclid(count as isize) as usize;
    }

    /// Puts `choice` in place of the mention being typed.
    pub fn complete(&mut self, choice: &str) {
        let start = self.text.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        self.text.truncate(start);
        self.text.push('@');
        self.text.push_str(choice);
        self.text.push(' ');
        self.chosen = 0;
    }

    /// The message as prompt parts: the text as typed, then one part per mention.
    pub fn parts(text: &str, root: &Path, selection: Option<Selection>) -> Vec<PromptPart> {
        let mut parts = vec![PromptPart::Text(text.to_string())];
        let mut seen = Vec::new();
        for word in text.split_whitespace() {
            let Some(name) = word.strip_prefix('@') else {
                continue;
            };
            let name = name.trim_end_matches([',', '.', ';', ':', ')']);
            if seen.contains(&name) {
                continue;
            }
            seen.push(name);
            if name == SELECTION {
                if let Some((path, lines, text)) = selection.clone() {
                    parts.push(PromptPart::Snippet { path, lines, text });
                }
                continue;
            }
            let file = root.join(name);
            if file.is_file() {
                parts.push(PromptPart::File(file));
            }
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_is_offered_and_completed() {
        let root = Path::new("C:/p");
        let files = [root.join("src/Main.txt"), root.join("pom.xml")];
        let mut composer = Composer::default();
        composer.insert("look at @ma");

        assert_eq!(composer.mention_query(), Some("ma"));
        assert_eq!(composer.candidates(root, &files), ["src/Main.txt"]);
        composer.complete("src/Main.txt");
        assert_eq!(composer.text, "look at @src/Main.txt ");
        assert_eq!(composer.mention_query(), None);

        composer.insert("@");
        assert_eq!(composer.candidates(root, &files)[0], SELECTION);
    }

    #[test]
    fn mentions_become_parts_and_unknown_ones_stay_text() {
        let root = std::env::temp_dir().join(format!("composer-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let selection = (root.join("b.txt"), (2, 3), "picked".to_string());

        let parts = Composer::parts("see @a.txt, @selection and @nope", &root, Some(selection));

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], PromptPart::File(root.join("a.txt")));
        assert!(matches!(
            &parts[2],
            PromptPart::Snippet { lines: (2, 3), .. }
        ));
    }
}
