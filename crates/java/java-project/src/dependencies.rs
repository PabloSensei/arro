//! What the tool reported about the dependencies of a module, and the change of a build
//! file that leaves an artifact out of a dependency declared there. The edits are plain
//! offsets of the text the caller holds: the window makes them in its editor, so one
//! undo takes the change back.

use std::path::Path;

use project_model::{Coordinates, Dependency, DependencyScope, Exclusion, FileEdit};
use serde::Deserialize;

/// The dependency trees of one module, one per scope.
#[derive(Default, Deserialize)]
pub(crate) struct ReportTrees {
    #[serde(default)]
    compile: Vec<ReportDependency>,
    #[serde(default)]
    runtime: Vec<ReportDependency>,
    #[serde(default)]
    test: Vec<ReportDependency>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportDependency {
    coordinates: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    requested: Option<String>,
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    path: Option<std::path::PathBuf>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    children: Vec<ReportDependency>,
}

impl From<ReportDependency> for Dependency {
    fn from(entry: ReportDependency) -> Self {
        Self {
            coordinates: entry.coordinates,
            scope: entry.scope,
            requested: entry.requested,
            winner: entry.winner,
            path: entry.path,
            module: entry.module,
            children: entry.children.into_iter().map(Into::into).collect(),
        }
    }
}

impl ReportTrees {
    /// The scopes that brought something, in the order the window shows them.
    pub(crate) fn into_scopes(self) -> Vec<DependencyScope> {
        [
            ("compile", self.compile),
            ("runtime", self.runtime),
            ("test", self.test),
        ]
        .into_iter()
        .filter(|(_, roots)| !roots.is_empty())
        .map(|(name, roots)| DependencyScope {
            name: name.to_owned(),
            roots: roots.into_iter().map(Into::into).collect(),
        })
        .collect()
    }
}

/// The insertion of an `<exclusion>` into the first-level dependency `request.declared`
/// of a `pom.xml`, before its closing tag.
pub(crate) fn exclude_in_pom(text: &str, request: &Exclusion, path: &Path) -> Option<FileEdit> {
    let declared = Coordinates::parse(&request.declared)?;
    let close = declared_dependency(text, &declared.artifact)?;
    let indent = indent_of(text, close);
    let inner = format!("{indent}    ");
    let text = format!(
        "{inner}<exclusion>\n{inner}    <groupId>{}</groupId>\n{inner}    <artifactId>{}</artifactId>\n{inner}</exclusion>\n",
        request.group, request.artifact,
    );
    let start = close - indent.len();
    Some(FileEdit {
        path: path.to_owned(),
        start,
        end: start,
        text,
    })
}

/// The start of the closing tag of the first `<dependency>` that declares `artifact`
/// outside `dependencyManagement`.
fn declared_dependency(text: &str, artifact: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find("<artifactId") {
        let open = from + found;
        let after = open + "<artifactId".len();
        let tag_end = after + text[after..].find('>')?;
        let value_end = tag_end + 1;
        let close = value_end + text[value_end..].find("</artifactId>")?;
        let value = text[value_end..close].trim();
        from = close + "</artifactId>".len();
        if value != artifact {
            continue;
        }
        if inside_management(text, open) {
            continue;
        }
        let start = text[..open].rfind("<dependency")?;
        let end = start + text[start..].find("</dependency>")?;
        if end < open {
            continue;
        }
        return Some(end);
    }
    None
}

/// Whether `at` lies inside a `<dependencyManagement>` element.
fn inside_management(text: &str, at: usize) -> bool {
    let opened = text[..at].rfind("<dependencyManagement");
    let closed = text[..at].rfind("</dependencyManagement");
    match opened {
        Some(opened) => closed.is_none_or(|closed| closed < opened),
        None => false,
    }
}

/// The spaces and tabs that start the line `at` lies on.
fn indent_of(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |found| found + 1);
    let line = &text[start..];
    let length = line
        .find(|character: char| character != ' ' && character != '\t')
        .unwrap_or(line.len());
    &line[..length]
}

/// The edit that adds `exclude(group = ..., module = ...)` to the dependency declared
/// by its coordinates in a build script. A declaration without a block gets one on the
/// same line; both DSLs accept the closure form.
pub(crate) fn exclude_in_script(text: &str, request: &Exclusion, path: &Path) -> Option<FileEdit> {
    let declared = Coordinates::parse(&request.declared)?;
    let needle = format!("{}:{}:", declared.group, declared.artifact);
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let at = from + found;
        from = at + needle.len();
        // The coordinates stand in a string literal: the quote before and after them.
        let Some(quote) = text[..at].rfind(['"', '\'']) else {
            continue;
        };
        if !text[quote + 1..at].trim().is_empty() {
            continue;
        }
        let Some(end) = text[at..].find(text[quote..].chars().next()?) else {
            continue;
        };
        let end = at + end;
        let line_end = text[end..]
            .find('\n')
            .map_or(text.len(), |found| end + found);
        let rest = text[end + 1..line_end].trim_end();
        let indent = indent_of(text, quote);
        let call = format!(
            "(group = \"{}\", module = \"{}\")",
            request.group, request.artifact
        );
        if let Some(open) = rest.find('{').map(|found| end + 1 + found) {
            // Already a block: the exclusion goes in before its closing brace.
            let close = matching_brace(text, open)?;
            let line_start = text[..close].rfind('\n').map_or(close, |found| found + 1);
            let inner = format!("{indent}    ");
            return Some(FileEdit {
                path: path.to_owned(),
                start: line_start,
                end: line_start,
                text: format!("{inner}exclude{call}\n"),
            });
        }
        // A one-line declaration takes the closure after it.
        return Some(FileEdit {
            path: path.to_owned(),
            start: line_end,
            end: line_end,
            text: format!(" {{ exclude{call} }}"),
        });
    }
    None
}

/// The index of the brace that closes the block opened at `open`.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (at, character) in text[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + at);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn exclusion(declared: &str) -> Exclusion {
        Exclusion {
            group: "com.fasterxml.jackson.core".into(),
            artifact: "jackson-core".into(),
            declared: declared.into(),
        }
    }

    const POM: &str = r#"<project>
    <dependencies>
        <dependency>
            <groupId>org.springframework.boot</groupId>
            <artifactId>spring-boot-starter-web</artifactId>
        </dependency>
    </dependencies>
</project>"#;

    #[test]
    fn the_exclusion_goes_into_the_declared_dependency() {
        let path = PathBuf::from("pom.xml");

        let edit = exclude_in_pom(
            POM,
            &exclusion("org.springframework.boot:spring-boot-starter-web:3.5.0"),
            &path,
        )
        .unwrap();

        let mut changed = POM.to_owned();
        changed.replace_range(edit.start..edit.end, &edit.text);
        assert!(
            changed.contains(
                "<artifactId>spring-boot-starter-web</artifactId>\n            <exclusion>"
            )
        );
        assert!(changed.contains("<artifactId>jackson-core</artifactId>"));
        assert!(changed.ends_with("</dependency>\n    </dependencies>\n</project>"));
    }

    #[test]
    fn a_dependency_of_the_management_section_is_not_a_declaration() {
        let text = r#"<project><dependencyManagement><dependencies>
            <dependency><artifactId>spring-web</artifactId></dependency>
            </dependencies></dependencyManagement>
            <dependencies>
            <dependency><groupId>g</groupId><artifactId>spring-web</artifactId></dependency>
            </dependencies></project>"#;

        let edit = exclude_in_pom(text, &exclusion("g:spring-web:1"), Path::new("pom.xml"));

        let edit = edit.unwrap();
        assert!(
            edit.start > text.find("</dependencyManagement>").unwrap(),
            "the direct declaration is the one that gets the exclusion"
        );
    }

    #[test]
    fn a_dependency_that_is_not_declared_gets_no_edit() {
        assert!(
            exclude_in_pom(POM, &exclusion("g:other:1"), Path::new("pom.xml")).is_none(),
            "a transitive dependency is excluded through the one that declares it"
        );
    }

    #[test]
    fn a_one_line_declaration_takes_a_closure() {
        let text = "dependencies {\n    implementation 'com.fasterxml.jackson.core:jackson-databind:2.17.0'\n}\n";

        let edit = exclude_in_script(
            text,
            &exclusion("com.fasterxml.jackson.core:jackson-databind:2.17.0"),
            Path::new("build.gradle"),
        )
        .unwrap();

        let mut changed = text.to_owned();
        changed.replace_range(edit.start..edit.end, &edit.text);
        assert_eq!(
            changed,
            "dependencies {\n    implementation 'com.fasterxml.jackson.core:jackson-databind:2.17.0' \
             { exclude(group = \"com.fasterxml.jackson.core\", module = \"jackson-core\") }\n}\n"
        );
    }

    #[test]
    fn a_block_declaration_gets_the_call_inside() {
        let text =
            "dependencies {\n    implementation(\"g:a:1\") {\n        because(\"x\")\n    }\n}\n";

        let edit =
            exclude_in_script(text, &exclusion("g:a:1"), Path::new("build.gradle.kts")).unwrap();

        let mut changed = text.to_owned();
        changed.replace_range(edit.start..edit.end, &edit.text);
        assert!(
            changed.contains("because(\"x\")\n        exclude(group = \"com.fasterxml.jackson.core\", module = \"jackson-core\")\n    }"),
            "{changed}"
        );
    }

    #[test]
    fn a_script_without_the_declaration_gets_no_edit() {
        let text = "dependencies {\n    implementation(\"g:b:1\")\n}\n";

        assert!(exclude_in_script(text, &exclusion("g:a:1"), Path::new("build.gradle")).is_none());
    }
}
