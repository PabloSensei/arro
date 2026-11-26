//! The command line that a shell reads at its prompt. A service that builds a command from
//! words gives them to [`shell_line`] with the dialect of the shell that reads the line:
//! every word then reaches the program unchanged, and no word can start a second command,
//! an editing key or a completion.

use std::fmt;

/// How a shell reads a line typed at its prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialect {
    /// Windows PowerShell and PowerShell 7.
    PowerShell,
    /// The command prompt of Windows.
    Cmd,
    /// `sh` and its family: Bash, Zsh, Git Bash and the shells of WSL.
    #[default]
    Posix,
}

impl Dialect {
    /// The dialect of the shell that a command line starts, by the name of its program. A
    /// quoted program is read up to its quote. A program that is not known is a Posix shell.
    pub fn of_shell(command_line: &str) -> Self {
        let line = command_line.trim_start();
        let program = match line.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => line.split_whitespace().next().unwrap_or_default(),
        };
        let file = program
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match file.strip_suffix(".exe").unwrap_or(file.as_str()) {
            "pwsh" | "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            _ => Self::Posix,
        }
    }
}

/// The command line that runs `words` in a shell of `dialect`: the first word is the
/// program and the others are its arguments. Every word is quoted for the dialect, so the
/// shell passes it on exactly. A word with a control character, or with a character that
/// the dialect cannot quote, refuses the whole line: the line is typed as it is, so a
/// character the shell would act on must not be let through.
pub fn shell_line(dialect: Dialect, words: &[String]) -> Result<String, QuoteError> {
    let quoted = words
        .iter()
        .map(|word| quote(dialect, word))
        .collect::<Result<Vec<String>, QuoteError>>()?;
    Ok(match dialect {
        // The call operator runs the first word, which is a program whose name is a string.
        Dialect::PowerShell if !quoted.is_empty() => format!("& {}", quoted.join(" ")),
        _ => quoted.join(" "),
    })
}

/// A word that cannot be typed into a shell of the dialect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteError {
    word: String,
    reason: &'static str,
}

impl fmt::Display for QuoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} cannot be typed into the shell: {}",
            self.word, self.reason
        )
    }
}

impl std::error::Error for QuoteError {}

fn quote(dialect: Dialect, word: &str) -> Result<String, QuoteError> {
    let refuse = |reason| QuoteError {
        word: word.to_owned(),
        reason,
    };
    if word.chars().any(char::is_control) {
        return Err(refuse("it has a control character"));
    }
    match dialect {
        Dialect::PowerShell => Ok(powershell(word)),
        Dialect::Cmd => cmd(word).ok_or_else(|| {
            refuse("the command prompt takes no quote, percent sign or exclamation mark in it")
        }),
        Dialect::Posix => Ok(posix(word)),
    }
}

/// A single-quoted PowerShell string, where nothing expands. PowerShell ends such a string
/// at the typographic single quotes as well, so each of them is doubled like the plain one.
fn powershell(word: &str) -> String {
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('\'');
    for c in word.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// A word of the command prompt. A word with a separator is in double quotes, where the
/// prompt takes the other characters as they are. A quote, a percent sign or an exclamation
/// mark is read by the prompt itself, so such a word has no safe form.
fn cmd(word: &str) -> Option<String> {
    if word.contains(['"', '%', '!']) {
        return None;
    }
    if word.is_empty() {
        return Some("\"\"".to_owned());
    }
    if word.contains([' ', '\t', '&', '|', '<', '>', '^', '(', ')', ',', ';', '=']) {
        return Some(format!("\"{word}\""));
    }
    Some(word.to_owned())
}

/// A word of `sh`: as it is when nothing in it needs quoting, else in single quotes, where
/// an inner single quote closes the quotes, is escaped and opens them again.
fn posix(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&byte));
    if plain {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_powershell_word_is_a_string_that_nothing_expands() {
        assert_eq!(
            shell_line(
                Dialect::PowerShell,
                &words(&["kubectl", "exec", "prod;calc"])
            )
            .unwrap(),
            "& 'kubectl' 'exec' 'prod;calc'"
        );
        assert_eq!(
            shell_line(Dialect::PowerShell, &words(&["tool", "a b"])).unwrap(),
            "& 'tool' 'a b'"
        );
        assert_eq!(
            shell_line(Dialect::PowerShell, &words(&["tool", "it's"])).unwrap(),
            "& 'tool' 'it''s'"
        );
        assert_eq!(
            shell_line(Dialect::PowerShell, &words(&["tool", "$(x)"])).unwrap(),
            "& 'tool' '$(x)'"
        );
    }

    #[test]
    fn a_typographic_quote_does_not_end_a_powershell_string() {
        assert_eq!(
            shell_line(Dialect::PowerShell, &words(&["tool", "a\u{2019}b"])).unwrap(),
            "& 'tool' 'a\u{2019}\u{2019}b'"
        );
    }

    #[test]
    fn a_posix_word_is_quoted_unless_it_is_plain() {
        assert_eq!(
            shell_line(
                Dialect::Posix,
                &words(&["kubectl", "a b", "it's", "$(x)", "plain-1.2"])
            )
            .unwrap(),
            "kubectl 'a b' 'it'\\''s' '$(x)' plain-1.2"
        );
        assert_eq!(shell_line(Dialect::Posix, &words(&[""])).unwrap(), "''");
    }

    #[test]
    fn a_cmd_word_with_a_separator_is_in_double_quotes() {
        assert_eq!(
            shell_line(
                Dialect::Cmd,
                &words(&["C:\\Program Files\\kubectl.exe", "get"])
            )
            .unwrap(),
            "\"C:\\Program Files\\kubectl.exe\" get"
        );
        assert_eq!(
            shell_line(Dialect::Cmd, &words(&["prod&calc"])).unwrap(),
            "\"prod&calc\""
        );
    }

    /// The prompt reads a quote, a percent sign and an exclamation mark itself: no quoting
    /// makes them safe, so the line is refused.
    #[test]
    fn a_cmd_word_with_what_the_prompt_reads_refuses_the_line() {
        assert!(shell_line(Dialect::Cmd, &words(&["a\"b"])).is_err());
        assert!(shell_line(Dialect::Cmd, &words(&["100%"])).is_err());
        assert!(shell_line(Dialect::Cmd, &words(&["a!b"])).is_err());
    }

    #[test]
    fn a_control_character_refuses_the_line_in_every_dialect() {
        for dialect in [Dialect::PowerShell, Dialect::Cmd, Dialect::Posix] {
            for word in ["a\rb", "a\nb", "a\tb", "a\u{1b}b", "a\u{7f}", "a\0b"] {
                assert!(
                    shell_line(dialect, &words(&[word])).is_err(),
                    "{dialect:?} takes {word:?}"
                );
            }
        }
    }

    #[test]
    fn the_dialect_of_a_shell_is_read_from_its_program() {
        assert_eq!(Dialect::of_shell("pwsh.exe -NoLogo"), Dialect::PowerShell);
        assert_eq!(
            Dialect::of_shell("\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\""),
            Dialect::PowerShell
        );
        assert_eq!(
            Dialect::of_shell("C:\\Windows\\System32\\cmd.exe"),
            Dialect::Cmd
        );
        assert_eq!(Dialect::of_shell("/bin/zsh -l"), Dialect::Posix);
        assert_eq!(Dialect::of_shell(""), Dialect::Posix);
    }

    /// Runs a line of hostile words in PowerShell, when the machine has it. Each word
    /// comes back as the text it was, and the command the word names does not run.
    #[test]
    #[ignore = "needs pwsh on the PATH"]
    fn a_hostile_powershell_word_runs_nothing() {
        let folder = test_support::TempDir::new("terminal-dialect-pwsh");
        let marker = folder.path().join("ran.txt");
        let hostile = format!("$(New-Item -ItemType File '{}')", marker.display());
        let typed = [
            "prod;calc".to_owned(),
            "a b".to_owned(),
            "it's".to_owned(),
            hostile.clone(),
        ];
        let mut line_words = vec!["Write-Output".to_owned()];
        line_words.extend(typed.iter().cloned());
        let line = shell_line(Dialect::PowerShell, &line_words).unwrap();

        let output = std::process::Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command", &line])
            .output()
            .expect("pwsh runs");

        assert!(output.status.success(), "{line}");
        let text = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines, typed.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(!marker.exists(), "the word ran as a command");
    }
}
