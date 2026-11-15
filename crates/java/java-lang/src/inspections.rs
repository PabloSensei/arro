//! The checks of Java: the ones this language runs itself and the compiler problems of
//! the server that a project setting can turn off or raise.

use lang_api::{InspectionInfo, InspectionLevel, InspectionSettings, SuppressionTarget};

/// The checks of the native providers. The ids are the codes their diagnostics carry.
const NATIVE: &[(&str, &str, &str, InspectionLevel)] = &[
    (
        "spring.config.unknownProperty",
        "Unknown Spring Boot property",
        "Spring",
        InspectionLevel::Warning,
    ),
    (
        "spring.config.deprecatedProperty",
        "Deprecated Spring Boot property",
        "Spring",
        InspectionLevel::Warning,
    ),
    (
        "spring.config.invalidValue",
        "Invalid Spring Boot property value",
        "Spring",
        InspectionLevel::Error,
    ),
    (
        "spring.data.methodQuery",
        "Repository method does not match its query",
        "Spring Data",
        InspectionLevel::Warning,
    ),
    (
        "spring.data.query",
        "Unresolved entity, property or parameter of a query",
        "Spring Data",
        InspectionLevel::Error,
    ),
    (
        "messages.unknownKey",
        "Cannot resolve property key",
        "Resource Bundles",
        InspectionLevel::Warning,
    ),
    (
        "messages.unusedProperty",
        "Unused property",
        "Resource Bundles",
        InspectionLevel::WeakWarning,
    ),
    (
        "messages.missingTranslation",
        "Missing translation",
        "Resource Bundles",
        InspectionLevel::Warning,
    ),
    (
        "beans.noCandidate",
        "No bean of the injected type",
        "Spring Beans",
        InspectionLevel::Warning,
    ),
    (
        "beans.multipleCandidates",
        "More than one bean of the injected type",
        "Spring Beans",
        InspectionLevel::Warning,
    ),
    (
        "build.dependencyUpdate",
        "Newer dependency version available",
        "Build Files",
        InspectionLevel::Info,
    ),
];

/// The compiler problems of Eclipse JDT that the server takes from its settings. The id
/// is the key of the `.prefs` file the server reads, and the title follows the names of
/// IDEA where they agree.
const JDT: &[(&str, &str, InspectionLevel)] = &[
    (
        "org.eclipse.jdt.core.compiler.problem.unusedImport",
        "Unused import",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedLocalVariable",
        "Unused local variable",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedPrivateMember",
        "Unused private member",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedParameter",
        "Unused parameter",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedDeclaredThrownException",
        "Unused declared exception",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedTypeParameter",
        "Unused type parameter",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedLabel",
        "Unused label",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unnecessaryCast",
        "Unnecessary cast",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unnecessaryElse",
        "Unnecessary else",
        InspectionLevel::WeakWarning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unnecessarySemicolon",
        "Unnecessary semicolon",
        InspectionLevel::WeakWarning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.redundantNullCheck",
        "Redundant null check",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.rawTypeReference",
        "Raw use of parameterized class",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.uncheckedTypeOperation",
        "Unchecked or unsafe operation",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.deprecation",
        "Deprecated member",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.missingOverrideAnnotation",
        "Missing @Override",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.nullReference",
        "Null pointer access",
        InspectionLevel::Error,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.potentialNullReference",
        "Potential null pointer access",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.deadCode",
        "Dead code",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.unusedObjectAllocation",
        "Unused object allocation",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.missingSerialVersion",
        "Missing serialVersionUID",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.noEffectAssignment",
        "Assignment has no effect",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.fallthroughCase",
        "Switch case falls through",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.indirectStaticAccess",
        "Static member accessed through an instance",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.hiddenCatchBlock",
        "Hidden catch block",
        InspectionLevel::Warning,
    ),
    (
        "org.eclipse.jdt.core.compiler.problem.parameterAssignment",
        "Assignment to a parameter",
        InspectionLevel::WeakWarning,
    ),
];

/// Every check of Java, for the Settings tree and the menu of a problem.
pub fn catalog() -> Vec<InspectionInfo> {
    NATIVE
        .iter()
        .map(|(id, title, group, severity)| InspectionInfo {
            id: (*id).to_owned(),
            title: (*title).to_owned(),
            group: (*group).to_owned(),
            default_severity: *severity,
            configurable: true,
        })
        .chain(JDT.iter().map(|(id, title, severity)| InspectionInfo {
            id: (*id).to_owned(),
            title: (*title).to_owned(),
            group: "Java".to_owned(),
            default_severity: *severity,
            configurable: true,
        }))
        .collect()
}

/// The check a diagnostic code names. The server gives the numeric `IProblem` ids of
/// Eclipse; a native provider gives its own id.
pub fn inspection_of(code: Option<&str>, message: &str) -> Option<InspectionInfo> {
    let code = code?;
    let catalog = catalog();
    if let Some(found) = catalog.iter().find(|info| info.id == code) {
        return Some(found.clone());
    }
    let key = jdt_key(code).or_else(|| key_from_message(message))?;
    catalog.into_iter().find(|info| info.id == key)
}

/// The `.prefs` lines of the settings the user changed. Only the compiler problems of
/// the server go here; the native checks are filtered in the editor.
pub fn jdt_prefs(settings: &InspectionSettings) -> Vec<(String, String)> {
    settings
        .inspections
        .iter()
        .filter(|(id, _)| id.starts_with("org.eclipse.jdt.core.compiler.problem."))
        .map(|(id, setting)| {
            let level = if setting.enabled {
                setting.level.unwrap_or(InspectionLevel::Warning)
            } else {
                InspectionLevel::Off
            };
            let value = match level {
                InspectionLevel::Error => "error",
                InspectionLevel::Warning | InspectionLevel::WeakWarning => "warning",
                InspectionLevel::Info => "info",
                InspectionLevel::Off => "ignore",
            };
            (id.clone(), value.to_owned())
        })
        .collect()
}

/// The token of `@SuppressWarnings` that silences the check of a diagnostic.
pub fn suppress_token(code: Option<&str>) -> Option<&'static str> {
    let code = code?;
    let key = if code.starts_with("org.eclipse.jdt.core.compiler.problem.") {
        code
    } else {
        jdt_key(code)?
    };
    Some(token_of(key))
}

/// The edit of a suppression of a check, for the editor.
pub fn suppression_edit(
    text: &str,
    offset: usize,
    code: Option<&str>,
    target: SuppressionTarget,
) -> Option<lang_api::DocumentEdit> {
    let token = suppress_token(code)?;
    let target = match target {
        SuppressionTarget::Statement => java_syntax::SuppressTarget::Statement,
        SuppressionTarget::Method => java_syntax::SuppressTarget::Method,
        SuppressionTarget::Class => java_syntax::SuppressTarget::Class,
    };
    let edit = java_syntax::suppression_edit(text, offset, target, token)?;
    Some(lang_api::DocumentEdit {
        span: span_in(text, edit.start, edit.end),
        text: edit.text,
    })
}

fn token_of(key: &str) -> &'static str {
    if key.contains("unused") || key.contains("unnecessary") || key.contains("deadCode") {
        "unused"
    } else if key.contains("rawType") {
        "rawtypes"
    } else if key.contains("unchecked") {
        "unchecked"
    } else if key.contains("deprecation") {
        "deprecation"
    } else if key.contains("Override") {
        "override"
    } else if key.contains("Null") {
        "null"
    } else if key.contains("SerialVersion") {
        "serial"
    } else if key.contains("fallthrough") {
        "fallthrough"
    } else if key.contains("StaticAccess") {
        "static-access"
    } else {
        "all"
    }
}

/// The key of the compiler problem of a numeric `IProblem` id.
fn jdt_key(code: &str) -> Option<&'static str> {
    let key = match code {
        "268435844" => "unusedImport",
        "536870973" => "unusedLocalVariable",
        "536870974" | "536870997" | "536871193" => "unusedParameter",
        "553648135" | "570425421" | "603979894" | "603979910" => "unusedPrivateMember",
        "16777777" | "16777788" | "2099067" => "rawTypeReference",
        "16777428" | "16777740" | "16777746" | "16777747" | "16777748" | "16777752"
        | "16777761" | "16777764" | "16777767" | "16777770" | "16777771" | "16777785"
        | "16777786" | "16777801" | "67109423" | "67109438" => "uncheckedTypeOperation",
        "16777221" | "16778616" | "16778621" | "16778626" | "16778649" | "33554505"
        | "33555835" | "33555840" | "33555845" | "67108967" | "67109276" | "134217861" => {
            "deprecation"
        }
        "67108927" => "missingOverrideAnnotation",
        "536870980" | "536871012" => "deadCode",
        "536871008" => "unusedObjectAllocation",
        "33554510" => "missingSerialVersion",
        _ => return None,
    };
    Some(match key {
        "unusedImport" => "org.eclipse.jdt.core.compiler.problem.unusedImport",
        "unusedLocalVariable" => "org.eclipse.jdt.core.compiler.problem.unusedLocalVariable",
        "unusedParameter" => "org.eclipse.jdt.core.compiler.problem.unusedParameter",
        "unusedPrivateMember" => "org.eclipse.jdt.core.compiler.problem.unusedPrivateMember",
        "rawTypeReference" => "org.eclipse.jdt.core.compiler.problem.rawTypeReference",
        "uncheckedTypeOperation" => "org.eclipse.jdt.core.compiler.problem.uncheckedTypeOperation",
        "deprecation" => "org.eclipse.jdt.core.compiler.problem.deprecation",
        "missingOverrideAnnotation" => {
            "org.eclipse.jdt.core.compiler.problem.missingOverrideAnnotation"
        }
        "deadCode" => "org.eclipse.jdt.core.compiler.problem.deadCode",
        "unusedObjectAllocation" => "org.eclipse.jdt.core.compiler.problem.unusedObjectAllocation",
        "missingSerialVersion" => "org.eclipse.jdt.core.compiler.problem.missingSerialVersion",
        _ => return None,
    })
}

fn key_from_message(message: &str) -> Option<&'static str> {
    if message.contains("never used") {
        Some("org.eclipse.jdt.core.compiler.problem.unusedPrivateMember")
    } else if message.contains("The import") && message.contains("is never used") {
        Some("org.eclipse.jdt.core.compiler.problem.unusedImport")
    } else {
        None
    }
}

fn span_in(text: &str, start: usize, end: usize) -> lang_api::TextSpan {
    lang_api::TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

fn point_in(text: &str, offset: usize) -> lang_api::TextPoint {
    let before = &text[..offset.min(text.len())];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset.min(text.len())]
        .encode_utf16()
        .count() as u32;
    lang_api::TextPoint { line, character }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_check_has_a_unique_id_and_the_native_ones_are_there() {
        let catalog = catalog();
        let mut ids = HashSet::new();
        for info in &catalog {
            assert!(ids.insert(info.id.clone()), "the id {} repeats", info.id);
            assert!(!info.title.is_empty());
            assert!(!info.group.is_empty());
        }
        for (id, _, _, _) in NATIVE {
            assert!(
                ids.contains(*id),
                "the native check {id} is not in the catalog"
            );
        }
        assert!(catalog.len() >= NATIVE.len() + 20);
        assert!(catalog.iter().any(|info| info.id.ends_with("unusedImport")));
    }

    #[test]
    fn the_compiler_problems_of_the_settings_become_prefs_lines() {
        let mut settings = InspectionSettings::default();
        settings.set(
            "org.eclipse.jdt.core.compiler.problem.unusedImport",
            false,
            None,
        );
        settings.set(
            "org.eclipse.jdt.core.compiler.problem.rawTypeReference",
            true,
            Some(InspectionLevel::Error),
        );

        let prefs = jdt_prefs(&settings);

        assert_eq!(
            prefs,
            vec![
                (
                    "org.eclipse.jdt.core.compiler.problem.rawTypeReference".to_owned(),
                    "error".to_owned()
                ),
                (
                    "org.eclipse.jdt.core.compiler.problem.unusedImport".to_owned(),
                    "ignore".to_owned()
                ),
            ]
        );
    }

    fn apply(text: &str, offset: usize, target: SuppressionTarget, code: &str) -> String {
        let edit = suppression_edit(text, offset, Some(code), target).unwrap();
        let start = lang_api::offset_in(text, edit.span.start);
        let end = lang_api::offset_in(text, edit.span.end);
        format!("{}{}{}", &text[..start], edit.text, &text[end..])
    }

    #[test]
    fn a_suppression_goes_on_a_statement_a_method_and_a_class() {
        let text = "class A {\n    void f() {\n        int x = 1;\n    }\n}\n";
        let unused = "org.eclipse.jdt.core.compiler.problem.unusedLocalVariable";
        let statement = apply(
            text,
            text.find("int x").unwrap(),
            SuppressionTarget::Statement,
            unused,
        );
        assert!(
            statement.contains("@SuppressWarnings(\"unused\")\n        int x"),
            "{statement}"
        );
        let method = apply(
            text,
            text.find("int x").unwrap(),
            SuppressionTarget::Method,
            unused,
        );
        assert!(
            method.contains("@SuppressWarnings(\"unused\")\n    void f()"),
            "{method}"
        );
        let class = apply(
            text,
            text.find("int x").unwrap(),
            SuppressionTarget::Class,
            unused,
        );
        assert!(
            class.contains("@SuppressWarnings(\"unused\")\nclass A"),
            "{class}"
        );
    }

    #[test]
    fn an_existing_annotation_takes_the_token_into_its_list() {
        let text = "class A {\n    @SuppressWarnings({\"a\"})\n    void f() {\n        int x = 1;\n    }\n}\n";
        let unused = "org.eclipse.jdt.core.compiler.problem.unusedLocalVariable";

        let merged = apply(
            text,
            text.find("int x").unwrap(),
            SuppressionTarget::Method,
            unused,
        );

        assert!(
            merged.contains("@SuppressWarnings({\"a\", \"unused\"})"),
            "{merged}"
        );
    }

    #[test]
    fn a_problem_code_names_the_check_the_menu_offers() {
        let info = inspection_of(Some("268435844"), "").unwrap();
        assert_eq!(
            info.id,
            "org.eclipse.jdt.core.compiler.problem.unusedImport"
        );
        assert_eq!(
            suppress_token(Some(
                "org.eclipse.jdt.core.compiler.problem.rawTypeReference"
            )),
            Some("rawtypes")
        );
    }
}
