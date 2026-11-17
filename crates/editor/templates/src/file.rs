//! The templates of new files: their text and the variables the text may use.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::overrides::TemplateOverrides;

/// The variables of a new-file template and what each one becomes, for the page that
/// edits the text.
pub const FILE_TEMPLATE_VARIABLES: &[(&str, &str)] = &[
    (
        "PACKAGE_NAME",
        "the package of the folder the file goes into",
    ),
    ("NAME", "the name of the file without its extension"),
    ("USER", "the name of the user who runs the IDE"),
    ("DATE", "today as YYYY-MM-DD"),
];

/// One kind of new file the project tree creates and the page edits.
#[derive(Clone, Debug)]
pub struct FileTemplateDefinition {
    /// The key of the text in `templates.json`.
    pub id: String,
    /// The name of the entry in the project tree.
    pub label: String,
    /// The text as the language ships it.
    pub default_text: String,
    /// The text in force, the text of the user or the default one.
    pub text: String,
    /// The package of the folder, from the project root and the folder.
    pub package_name: fn(&Path, &Path) -> String,
}

impl FileTemplateDefinition {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        text: impl Into<String>,
        package_name: fn(&Path, &Path) -> String,
    ) -> Self {
        let text = text.into();
        Self {
            id: id.into(),
            label: label.into(),
            default_text: text.clone(),
            text,
            package_name,
        }
    }

    /// Takes the text of the user over the default one.
    pub fn apply(&mut self, overrides: &TemplateOverrides) {
        self.text = overrides.file_or_default(&self.id, &self.default_text);
    }

    /// Whether the user changed the text.
    pub fn is_changed(&self, overrides: &TemplateOverrides) -> bool {
        overrides.file_text(&self.id).is_some()
    }

    /// The text of the new file, with every variable filled in.
    pub fn render(&self, root: &Path, folder: &Path, name: &str) -> String {
        let package = (self.package_name)(root, folder);
        render_file_template(&self.text, &package, name, &user_name(), &today())
    }
}

/// The name of the user, as `${USER}` shows it.
pub fn user_name() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

/// Today as `YYYY-MM-DD`, as `${DATE}` shows it.
pub fn today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The date of a count of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// Fills the variables of the text of a new file. A line `package ${PACKAGE_NAME};` goes
/// away with the blank line after it when the file has no package.
pub fn render_file_template(
    text: &str,
    package: &str,
    name: &str,
    user: &str,
    date: &str,
) -> String {
    let mut filled = String::with_capacity(text.len());
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if package.is_empty() && line.trim() == "package ${PACKAGE_NAME};" {
            if lines.peek().is_some_and(|next| next.trim().is_empty()) {
                lines.next();
            }
            continue;
        }
        filled.push_str(line);
        filled.push('\n');
    }
    if !text.ends_with('\n') && filled.ends_with('\n') {
        filled.pop();
    }
    filled
        .replace("${PACKAGE_NAME}", package)
        .replace("${NAME}", name)
        .replace("${USER}", user)
        .replace("${DATE}", date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, folder: &Path) -> String {
        folder
            .strip_prefix(root)
            .map(|rest| rest.to_string_lossy().replace(['/', '\\'], "."))
            .unwrap_or_default()
    }

    #[test]
    fn the_variables_are_filled_and_an_empty_package_takes_its_line_away() {
        let definition = FileTemplateDefinition::new(
            "class",
            "Class",
            "package ${PACKAGE_NAME};\n\npublic class ${NAME} {\n}\n",
            package,
        );
        assert_eq!(
            render_file_template(&definition.text, "com.shop", "Order", "me", "2026-01-02"),
            "package com.shop;\n\npublic class Order {\n}\n"
        );
        assert_eq!(
            render_file_template(&definition.text, "", "Order", "me", "2026-01-02"),
            "public class Order {\n}\n"
        );
    }

    #[test]
    fn the_date_is_the_civil_date_of_the_count_of_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }
}
