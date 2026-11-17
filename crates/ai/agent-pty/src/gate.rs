//! A whitelist of screens that may receive automatic input. Login, trust, theme and sandbox
//! screens never match, whatever their wording, so the IDE never presses anything there.

use agent_core::Harness;

const CODEX_PLACEHOLDER: &str = "› Ask Codex to do anything";

fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 20 && line.chars().all(|ch| ch == '─')
}

/// `❯ 1. Yes` and `› 1. Set up` are menu rows, the same glyphs mark the input box.
fn is_menu_row(line: &str) -> bool {
    let text = line.trim_start_matches(['❯', '›', ' ']);
    let numbered = text.starts_with(|ch: char| ch.is_ascii_digit()) && text.contains(". ");
    text.starts_with('✔') || numbered
}

/// `first` is the stricter check for the first prompt of a session, later a composer full of text
/// hides the placeholder. `marker` must be somewhere on screen when given: tests pass the mock
/// model name, which proves the throwaway config is in effect.
pub fn input_box_visible(
    harness: Harness,
    lines: &[String],
    first: bool,
    marker: Option<&str>,
) -> bool {
    let menu = |line: &String| line.starts_with(['❯', '›']) && is_menu_row(line);
    if lines.iter().any(menu) {
        return false;
    }
    if let Some(marker) = marker
        && !lines.iter().any(|line| line.contains(marker))
    {
        return false;
    }
    match harness {
        Harness::Claude => lines.iter().enumerate().any(|(index, line)| {
            line.starts_with('❯')
                && index > 0
                && is_rule(&lines[index - 1])
                && lines[index + 1..].iter().any(|below| is_rule(below))
        }),
        Harness::Codex => {
            let composer = |line: &String| {
                line.starts_with(CODEX_PLACEHOLDER) || (!first && line.starts_with("› "))
            };
            lines.iter().any(composer)
        }
        // Nobody has looked at its screens yet.
        Harness::OpenCode => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    const RULE: &str = "────────────────────────────────";

    #[test]
    fn claude_needs_the_prompt_between_rules() {
        let good = screen(&format!("welcome\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts"));

        assert!(input_box_visible(Harness::Claude, &good, true, None));
        assert!(!input_box_visible(
            Harness::Claude,
            &screen("❯ \nno rules"),
            true,
            None
        ));
    }

    #[test]
    fn menus_never_pass_even_inside_rules() {
        for menu in [
            format!("{RULE}\n❯ 1. Yes, proceed\n  2. No\n{RULE}"),
            format!("{RULE}\n❯ ✔ Dark mode\n{RULE}"),
            "› 1. Sign in with ChatGPT\n  2. Provide your own API key".to_string(),
            "› 1. Set up default sandbox\n› Ask Codex to do anything".to_string(),
        ] {
            let lines = screen(&menu);
            assert!(!input_box_visible(Harness::Claude, &lines, false, None));
            assert!(!input_box_visible(Harness::Codex, &lines, false, None));
        }
    }

    #[test]
    fn codex_shows_its_placeholder_on_the_first_prompt() {
        let empty = screen("› Ask Codex to do anything\n  gpt-5");
        let typed = screen("› half a sentence\n  gpt-5");

        assert!(input_box_visible(Harness::Codex, &empty, true, None));
        assert!(!input_box_visible(Harness::Codex, &typed, true, None));
        assert!(input_box_visible(Harness::Codex, &typed, false, None));
    }

    #[test]
    fn a_marker_has_to_be_on_screen() {
        let lines = screen("› Ask Codex to do anything\n  gpt-5");

        assert!(!input_box_visible(
            Harness::Codex,
            &lines,
            true,
            Some("mock-model")
        ));
    }

    #[test]
    fn a_login_screen_never_passes() {
        let lines = screen("Welcome to Codex\nPress Enter to continue\n");

        assert!(!input_box_visible(Harness::Codex, &lines, false, None));
        assert!(!input_box_visible(Harness::Claude, &lines, false, None));
    }

    #[test]
    fn a_harness_without_known_screens_takes_nothing() {
        let lines = screen(&format!("{RULE}\n❯ \n{RULE}"));

        assert!(!input_box_visible(Harness::OpenCode, &lines, false, None));
    }
}
