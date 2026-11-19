//! The change of a build file that sets the language level of a project or of one
//! module. The language knows where a level lives: a Maven property or the compiler
//! plugin, a Gradle toolchain or a source compatibility line. Everything else gets the
//! place it lacks, so that the window only has to pass the number.

use std::path::Path;

use lang_api::{DocumentEdit, SourceEdit, TextPoint, TextSpan, WorkspaceChange};
use platform::CanonPath;
use project_model::LanguageLevelEdit;

/// One text change in the coordinates of the file before the change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    start: usize,
    end: usize,
    text: String,
}

/// The changes of one build file and what the form says about them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Built {
    changes: Vec<Change>,
    /// The line the value takes in the changed file, counted from zero.
    line: u32,
    summary: String,
    added: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Pom,
    Groovy,
    Kotlin,
}

fn kind_of(file: &Path) -> Option<Kind> {
    let name = file.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("pom.xml") {
        Some(Kind::Pom)
    } else if name.eq_ignore_ascii_case("build.gradle") {
        Some(Kind::Groovy)
    } else if name.eq_ignore_ascii_case("build.gradle.kts") {
        Some(Kind::Kotlin)
    } else {
        None
    }
}

/// The change that sets `level` in the build file at `file`, whose current text is
/// `text`. An error is the text the form shows under the field.
pub fn set_language_level(
    file: &Path,
    text: &str,
    level: u32,
) -> Result<LanguageLevelEdit, String> {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let built = match kind_of(file) {
        Some(Kind::Pom) => pom_edit(text, level, &name)?,
        Some(Kind::Groovy) | Some(Kind::Kotlin) => gradle_edit(text, level, &name)?,
        None => {
            return Err(format!(
                "{} is not a build file whose language level this window knows",
                file.display()
            ));
        }
    };
    let uri = CanonPath::new(file)
        .map_err(|error| error.to_string())?
        .to_uri();
    let edits = built
        .changes
        .iter()
        .map(|change| DocumentEdit {
            span: TextSpan {
                start: point_of(text, change.start),
                end: point_of(text, change.end),
            },
            text: change.text.clone(),
        })
        .collect();
    Ok(LanguageLevelEdit {
        file: file.to_path_buf(),
        line: Some(built.line),
        summary: built.summary,
        added: built.added,
        change: WorkspaceChange {
            edits: vec![SourceEdit { uri, edits }],
            renames: Vec::new(),
        },
    })
}

fn eol(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// The zero-based line of `at`.
fn line_of(text: &str, at: usize) -> u32 {
    text[..at.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
}

/// The indentation of the line `at` is on.
fn line_indent(text: &str, at: usize) -> &str {
    let start = text[..at.min(text.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = &text[start..];
    let length = line.len() - line.trim_start_matches([' ', '\t']).len();
    &text[start..start + length]
}

fn point_of(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut start = 0usize;
    for (index, character) in text.char_indices() {
        if index >= offset {
            break;
        }
        if character == '\n' {
            line += 1;
            start = index + character.len_utf8();
        }
    }
    let character = text[start..offset]
        .chars()
        .map(|character| character.len_utf16() as u32)
        .sum();
    TextPoint { line, character }
}

/// An element of an XML text: where its tags are.
struct Element {
    open_start: usize,
    open_end: usize,
    close_start: usize,
}

fn element(text: &str, tag: &str, from: usize, to: usize) -> Option<Element> {
    let open = format!("<{tag}>");
    let open_start = from + text[from..to].find(&open)?;
    let open_end = open_start + open.len();
    let close = format!("</{tag}>");
    let close_start = open_end + text[open_end..to].find(&close)?;
    Some(Element {
        open_start,
        open_end,
        close_start,
    })
}

fn digits(text: &str) -> &str {
    let end = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    &text[..end]
}

fn digits_with_underscore(text: &str) -> &str {
    let end = text
        .find(|character: char| !character.is_ascii_digit() && character != '_')
        .unwrap_or(text.len());
    &text[..end]
}

/// `JavaVersion.VERSION_1_8` keeps the only spelling Java 8 has, every other level is
/// its number.
fn java_version_constant(level: u32) -> String {
    if level == 8 {
        "1_8".to_owned()
    } else {
        level.to_string()
    }
}

/// The level of the `maven-compiler-plugin` configuration and the zero-based line of
/// its value, when the pom sets one there.
pub(crate) fn plugin_level(text: &str) -> Option<(u32, u32)> {
    let marker = "<artifactId>maven-compiler-plugin</artifactId>";
    let at = text.find(marker)?;
    let plugin_start = text[..at].rfind("<plugin>").unwrap_or(at);
    let plugin_end = at + text[at..].find("</plugin>")? + "</plugin>".len();
    let configuration = element(text, "configuration", plugin_start, plugin_end)?;
    let inside = configuration.open_end..configuration.close_start;
    for tag in ["release", "source", "target"] {
        let Some(found) = element(text, tag, inside.start, inside.end) else {
            continue;
        };
        let level = crate::jdk::feature_of(text[found.open_end..found.close_start].trim())?;
        return Some((level, line_of(text, found.open_end)));
    }
    None
}

/// `maven.compiler.release`, then the compiler plugin, then the same source and target
/// properties: whichever the pom already uses. A pom that names none gets the property.
fn pom_edit(text: &str, level: u32, name: &str) -> Result<Built, String> {
    if let Some(found) = property_edit(text, level, name, &["maven.compiler.release"]) {
        return Ok(found);
    }
    if let Some(found) = compiler_plugin_edit(text, level, name) {
        return Ok(found);
    }
    if let Some(found) = property_edit(
        text,
        level,
        name,
        &["maven.compiler.source", "maven.compiler.target"],
    ) {
        return Ok(found);
    }
    insert_property(text, level, name)
}

fn compiler_plugin_edit(text: &str, level: u32, name: &str) -> Option<Built> {
    let marker = "<artifactId>maven-compiler-plugin</artifactId>";
    let at = text.find(marker)?;
    let plugin_start = text[..at].rfind("<plugin>").unwrap_or(at);
    let plugin_end = at + text[at..].find("</plugin>")? + "</plugin>".len();
    let configuration = element(text, "configuration", plugin_start, plugin_end)?;
    let inside = configuration.open_end..configuration.close_start;
    let mut changes = Vec::new();
    for tag in ["release", "source", "target"] {
        let Some(found) = element(text, tag, inside.start, inside.end) else {
            continue;
        };
        // Only the value of the tag, not the whitespace around it.
        let raw = &text[found.open_end..found.close_start];
        let value = raw.trim();
        let start = found.open_end + (raw.len() - value.len());
        changes.push(Change {
            start,
            end: start + value.len(),
            text: level.to_string(),
        });
        // A release decides alone; without one the source and the target go together.
        if tag == "release" {
            break;
        }
    }
    if changes.is_empty() {
        return None;
    }
    let tags: Vec<&str> = if element(text, "release", inside.start, inside.end).is_some() {
        vec!["release"]
    } else {
        ["source", "target"]
            .into_iter()
            .filter(|tag| element(text, tag, inside.start, inside.end).is_some())
            .collect()
    };
    let line = line_of(text, changes[0].start);
    Some(Built {
        changes,
        line,
        summary: format!(
            "Changes maven-compiler-plugin {} in {name} to {level}",
            tags.join(" and ")
        ),
        added: false,
    })
}

fn property_edit(text: &str, level: u32, name: &str, keys: &[&str]) -> Option<Built> {
    let mut changes = Vec::new();
    for tag in keys {
        let Some(found) = element(text, tag, 0, text.len()) else {
            continue;
        };
        let raw = &text[found.open_end..found.close_start];
        let value = raw.trim();
        let start = found.open_end + (raw.len() - value.len());
        changes.push(Change {
            start,
            end: start + value.len(),
            text: level.to_string(),
        });
    }
    if changes.is_empty() {
        return None;
    }
    let tags: Vec<&str> = keys
        .iter()
        .filter(|tag| element(text, tag, 0, text.len()).is_some())
        .copied()
        .collect();
    let line = line_of(text, changes[0].start);
    Some(Built {
        changes,
        line,
        summary: format!("Changes {} in {name} to {level}", tags.join(" and ")),
        added: false,
    })
}

fn insert_property(text: &str, level: u32, name: &str) -> Result<Built, String> {
    let newline = eol(text);
    let value = format!("<maven.compiler.release>{level}</maven.compiler.release>");
    let (at, snippet) = if let Some(properties) = element(text, "properties", 0, text.len()) {
        let open_indent = line_indent(text, properties.open_start).to_owned();
        let close_line = text[..properties.close_start]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let alone = text[close_line..properties.close_start].trim().is_empty();
        let inner = format!("{open_indent}    ");
        if alone {
            (close_line, format!("{inner}{value}{newline}"))
        } else {
            (
                properties.close_start,
                format!("{newline}{inner}{value}{newline}{open_indent}"),
            )
        }
    } else {
        let close = text
            .rfind("</project>")
            .ok_or_else(|| "The pom has no </project>".to_owned())?;
        let line_start = text[..close].rfind('\n').map_or(0, |index| index + 1);
        let indent = line_indent(text, close).to_owned();
        let alone = text[line_start..close].trim().is_empty();
        let properties_indent = if alone {
            format!("{indent}  ")
        } else {
            "  ".to_owned()
        };
        let snippet = format!(
            "{properties_indent}<properties>{newline}{properties_indent}    {value}{newline}{properties_indent}</properties>{newline}"
        );
        if alone {
            (line_start, snippet)
        } else {
            (close, format!("{newline}{snippet}"))
        }
    };
    let digits = format!(">{level}</");
    let value_at = snippet.find(&digits).map_or(0, |at| at + 1);
    Ok(Built {
        line: line_of(text, at) + snippet[..value_at].matches('\n').count() as u32,
        changes: vec![Change {
            start: at,
            end: at,
            text: snippet,
        }],
        summary: format!("Adds maven.compiler.release to {name}"),
        added: true,
    })
}

/// A toolchain, an older `JavaVersion` constant or a source compatibility line,
/// whichever the script already uses. A script that names none gets a toolchain block.
fn gradle_edit(text: &str, level: u32, name: &str) -> Result<Built, String> {
    if let Some(at) = text.find("JavaLanguageVersion.of(") {
        let start = at + "JavaLanguageVersion.of(".len();
        let run = digits(&text[start..]);
        if !run.is_empty() {
            return Ok(replacement(
                text,
                start,
                start + run.len(),
                level.to_string(),
                format!("Changes the java toolchain in {name} to {level}"),
            ));
        }
    }
    if let Some(found) = source_compatibility(text) {
        return Ok(replacement(
            text,
            found.0,
            found.1,
            if found.2 {
                java_version_constant(level)
            } else {
                level.to_string()
            },
            format!("Changes sourceCompatibility in {name} to {level}"),
        ));
    }
    if let Some(at) = text.find("JavaVersion.VERSION_") {
        let start = at + "JavaVersion.VERSION_".len();
        let run = digits_with_underscore(&text[start..]);
        if !run.is_empty() {
            return Ok(replacement(
                text,
                start,
                start + run.len(),
                java_version_constant(level),
                format!("Changes JavaVersion in {name} to {level}"),
            ));
        }
    }
    Ok(insert_toolchain(text, level, name))
}

fn replacement(text: &str, start: usize, end: usize, value: String, summary: String) -> Built {
    Built {
        changes: vec![Change {
            start,
            end,
            text: value,
        }],
        line: line_of(text, start),
        summary,
        added: false,
    }
}

/// The byte range of the value of `sourceCompatibility` and whether it is a
/// `JavaVersion.VERSION_` constant.
fn source_compatibility(text: &str) -> Option<(usize, usize, bool)> {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if let Some(found) = line.find("sourceCompatibility") {
            let after = offset + found + "sourceCompatibility".len();
            let rest = &text[after..offset + line.len()];
            if let Some(equals) = rest.find('=') {
                let value = &text[after + equals + 1..offset + line.len()];
                let trimmed = value.trim_start();
                let start = after + equals + 1 + (value.len() - trimmed.len());
                if let Some(constant) = trimmed.find("JavaVersion.VERSION_") {
                    let digits = start + constant + "JavaVersion.VERSION_".len();
                    let run = digits_with_underscore(&text[digits..]);
                    return (!run.is_empty()).then_some((digits, digits + run.len(), true));
                }
                let digits_at = start
                    + (trimmed.len()
                        - trimmed
                            .trim_start_matches(|c: char| !c.is_ascii_digit())
                            .len());
                let run = digits(&text[digits_at..]);
                return (!run.is_empty()).then_some((digits_at, digits_at + run.len(), false));
            }
        }
        offset += line.len();
    }
    None
}

fn insert_toolchain(text: &str, level: u32, name: &str) -> Built {
    let newline = eol(text);
    let mut snippet = String::new();
    if !text.is_empty() {
        if !text.ends_with('\n') {
            snippet.push_str(newline);
        }
        if !text.ends_with("\n\n") && !text.ends_with("\r\n\r\n") {
            snippet.push_str(newline);
        }
    }
    snippet.push_str(&format!(
        "java {{{newline}    toolchain {{{newline}        languageVersion = JavaLanguageVersion.of({level}){newline}    }}{newline}}}{newline}"
    ));
    let value_at = snippet.find(&format!("of({level})")).map_or(0, |at| at + 3);
    Built {
        line: line_of(text, text.len()) + snippet[..value_at].matches('\n').count() as u32,
        changes: vec![Change {
            start: text.len(),
            end: text.len(),
            text: snippet,
        }],
        summary: format!("Adds a java toolchain block to {name}"),
        added: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, built: &Built) -> String {
        let mut out = text.to_owned();
        let mut changes = built.changes.clone();
        changes.sort_by_key(|change| change.start);
        for change in changes.iter().rev() {
            out.replace_range(change.start..change.end, &change.text);
        }
        out
    }

    fn edit(name: &str, text: &str, level: u32) -> Built {
        match kind_of(Path::new(name)).unwrap() {
            Kind::Pom => pom_edit(text, level, name).unwrap(),
            Kind::Groovy | Kind::Kotlin => gradle_edit(text, level, name).unwrap(),
        }
    }

    #[test]
    fn a_release_property_takes_the_new_level() {
        let text = "<project><properties><maven.compiler.release>17</maven.compiler.release>\
                    </properties></project>";
        let built = edit("pom.xml", text, 21);

        assert!(!built.added);
        assert!(
            built.summary.contains("maven.compiler.release"),
            "{}",
            built.summary
        );
        assert!(
            apply(text, &built).contains("<maven.compiler.release>21</maven.compiler.release>")
        );
    }

    #[test]
    fn source_and_target_of_the_compiler_plugin_take_the_new_level() {
        let text = "<project><build><plugins><plugin>\
                    <artifactId>maven-compiler-plugin</artifactId><configuration>\
                    <source>17</source><target>17</target>\
                    </configuration></plugin></plugins></build></project>";
        let built = edit("pom.xml", text, 21);

        assert!(!built.added);
        let changed = apply(text, &built);
        assert!(changed.contains("<source>21</source>"), "{changed}");
        assert!(changed.contains("<target>21</target>"), "{changed}");
    }

    #[test]
    fn source_and_target_properties_take_the_new_level() {
        let text = "<project><properties><maven.compiler.source>17</maven.compiler.source>\
                    <maven.compiler.target>17</maven.compiler.target></properties></project>";
        let built = edit("pom.xml", text, 21);

        let changed = apply(text, &built);
        assert!(changed.contains("<maven.compiler.source>21</maven.compiler.source>"));
        assert!(changed.contains("<maven.compiler.target>21</maven.compiler.target>"));
    }

    #[test]
    fn a_pom_without_properties_gets_the_release_property() {
        let text = "<project>\n  <modelVersion>4.0.0</modelVersion>\n</project>\n";
        let built = edit("pom.xml", text, 21);

        assert!(built.added);
        let changed = apply(text, &built);
        assert!(changed.contains("<properties>"), "{changed}");
        assert!(
            changed.contains("<maven.compiler.release>21</maven.compiler.release>"),
            "{changed}"
        );
        assert!(changed.contains("  <properties>"), "{changed}");
        assert!(
            built.summary.contains("Adds maven.compiler.release"),
            "{}",
            built.summary
        );
    }

    #[test]
    fn the_release_property_wins_over_the_plugin_configuration() {
        let text = "<project><properties>\
                    <maven.compiler.release>17</maven.compiler.release></properties>\
                    <build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId>\
                    <configuration><source>11</source></configuration>\
                    </plugin></plugins></build></project>";
        let built = edit("pom.xml", text, 21);

        let changed = apply(text, &built);
        assert!(
            changed.contains("<maven.compiler.release>21</maven.compiler.release>"),
            "{changed}"
        );
        assert!(changed.contains("<source>11</source>"), "{changed}");
    }

    #[test]
    fn an_existing_properties_element_keeps_its_other_entries() {
        let text = "<project>\n  <properties>\n    <project.build.sourceEncoding>UTF-8\
                    </project.build.sourceEncoding>\n  </properties>\n</project>\n";
        let built = edit("pom.xml", text, 21);

        let changed = apply(text, &built);
        assert!(
            changed.contains("project.build.sourceEncoding"),
            "{changed}"
        );
        assert!(
            changed.contains("    <maven.compiler.release>21</maven.compiler.release>"),
            "{changed}"
        );
        assert!(changed.contains("  </properties>"), "{changed}");
    }

    #[test]
    fn a_gradle_toolchain_takes_the_new_level() {
        let text = "plugins { id 'java' }\n\
                    java { toolchain { languageVersion = JavaLanguageVersion.of(17) } }\n";
        let built = edit("build.gradle", text, 21);

        assert!(!built.added);
        assert!(apply(text, &built).contains("JavaLanguageVersion.of(21)"));
    }

    #[test]
    fn a_kotlin_script_keeps_its_source_compatibility() {
        let text = "java {\n    sourceCompatibility = JavaVersion.VERSION_17\n}\n";
        let built = edit("build.gradle.kts", text, 21);

        assert!(!built.added);
        assert!(
            built.summary.contains("sourceCompatibility"),
            "{}",
            built.summary
        );
        assert!(apply(text, &built).contains("sourceCompatibility = JavaVersion.VERSION_21"));
    }

    #[test]
    fn a_groovy_script_keeps_its_source_compatibility() {
        let text = "plugins { id 'java' }\nsourceCompatibility = 17\n";
        let built = edit("build.gradle", text, 21);

        assert!(apply(text, &built).contains("sourceCompatibility = 21"));
    }

    #[test]
    fn java_eight_keeps_the_underscore_spelling() {
        let text = "java {\n    sourceCompatibility = JavaVersion.VERSION_17\n}\n";
        let built = edit("build.gradle.kts", text, 8);

        assert!(apply(text, &built).contains("JavaVersion.VERSION_1_8"));
    }

    #[test]
    fn a_script_without_a_level_gets_a_toolchain_block() {
        let text = "plugins { id 'java' }\n";
        let built = edit("build.gradle.kts", text, 21);

        assert!(built.added);
        let changed = apply(text, &built);
        assert!(
            changed.contains(
                "java {\n    toolchain {\n        languageVersion = JavaLanguageVersion.of(21)"
            ),
            "{changed}"
        );
        assert!(
            built.summary.contains("Adds a java toolchain block"),
            "{}",
            built.summary
        );
    }

    #[test]
    fn the_change_has_the_uri_of_the_file_and_the_line_of_the_value() {
        let root = test_support::scratch("java-project-level-uri");
        let file = root.join("pom.xml");
        std::fs::write(
            &file,
            "<project>\n  <properties>\n    <maven.compiler.release>17</maven.compiler.release>\n  </properties>\n</project>\n",
        )
        .unwrap();

        let edit = set_language_level(&file, &std::fs::read_to_string(&file).unwrap(), 21).unwrap();

        assert!(edit.file.ends_with("pom.xml"));
        assert_eq!(edit.line, Some(2));
        assert_eq!(edit.change.edits.len(), 1);
        assert!(
            edit.change.edits[0].uri.starts_with("file:///"),
            "{}",
            edit.change.edits[0].uri
        );
        assert!(
            !lang_api::apply_to_text(
                &std::fs::read_to_string(&file).unwrap(),
                &edit.change.edits[0].edits
            )
            .contains(">17<")
        );
    }

    #[test]
    fn a_file_that_is_not_a_build_file_is_refused() {
        let error = set_language_level(Path::new("notes.txt"), "text", 21)
            .err()
            .unwrap_or_default();
        assert!(error.contains("not a build file"), "{error}");
    }
}
