//! The replacements `propose_edit` takes: each `old_text` has to stand exactly once in the
//! text it works on, so that a wrong edit is an error and not a guess.

use crate::backend::{TextReplacement, ToolError};

/// A short one-line picture of a piece of text for an error message.
fn preview(text: &str) -> String {
    let mut shown: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect();
    if text.lines().count() > 1 || text.chars().count() > 60 {
        shown.push_str("...");
    }
    shown
}

/// The text with every replacement applied in order. A `old_text` that is missing or
/// stands more than once is an error: the edit would be a guess.
pub fn apply_replacements(
    text: &str,
    replacements: &[TextReplacement],
) -> Result<String, ToolError> {
    let mut result = text.to_string();
    for (index, replacement) in replacements.iter().enumerate() {
        let number = index + 1;
        if replacement.old_text.is_empty() {
            return Err(ToolError::BadInput(format!(
                "replacement {number}: old_text is empty"
            )));
        }
        let mut found = result.match_indices(&replacement.old_text);
        let Some((at, _)) = found.next() else {
            return Err(ToolError::BadInput(format!(
                "replacement {number}: old_text was not found: {:?}",
                preview(&replacement.old_text)
            )));
        };
        if found.next().is_some() {
            return Err(ToolError::BadInput(format!(
                "replacement {number}: old_text stands more than once, add the lines around it: {:?}",
                preview(&replacement.old_text)
            )));
        }
        let end = at + replacement.old_text.len();
        result.replace_range(at..end, &replacement.new_text);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::apply_replacements;
    use crate::backend::{TextReplacement, ToolError};

    fn replacement(old_text: &str, new_text: &str) -> TextReplacement {
        TextReplacement {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn replacements_apply_one_after_another() {
        let text = "alpha\nbeta\ngamma\n";

        let edited = apply_replacements(
            text,
            &[replacement("alpha", "ALPHA"), replacement("gamma", "GAMMA")],
        )
        .expect("the replacements apply");

        assert_eq!(edited, "ALPHA\nbeta\nGAMMA\n");
    }

    #[test]
    fn a_missing_old_text_is_an_error_with_its_place() {
        let error = apply_replacements("alpha\n", &[replacement("omega", "x")]).unwrap_err();

        assert!(matches!(error, ToolError::BadInput(_)), "{error:?}");
        assert!(error.to_string().contains("replacement 1"), "{error}");
        assert!(error.to_string().contains("was not found"), "{error}");
        assert!(error.to_string().contains("omega"), "{error}");
    }

    #[test]
    fn an_old_text_that_stands_twice_is_an_error() {
        let error = apply_replacements("foo\nbar\nfoo\n", &[replacement("foo", "x")]).unwrap_err();

        assert!(matches!(error, ToolError::BadInput(_)), "{error:?}");
        assert!(error.to_string().contains("more than once"), "{error}");
        assert!(error.to_string().contains("foo"), "{error}");
    }

    #[test]
    fn an_empty_old_text_is_an_error() {
        let error = apply_replacements("alpha\n", &[replacement("", "x")]).unwrap_err();

        assert!(error.to_string().contains("old_text is empty"), "{error}");
    }
}
