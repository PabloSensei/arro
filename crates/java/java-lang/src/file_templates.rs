//! The templates of new Java files. The text uses the variables of the neutral
//! `templates` crate; the user changes it on the Settings page.

use templates::FileTemplateDefinition;

use crate::package_of;

const CLASS: &str = "package ${PACKAGE_NAME};\n\npublic class ${NAME} {\n}\n";
const INTERFACE: &str = "package ${PACKAGE_NAME};\n\npublic interface ${NAME} {\n}\n";
const ENUM: &str = "package ${PACKAGE_NAME};\n\npublic enum ${NAME} {\n}\n";
const RECORD: &str = "package ${PACKAGE_NAME};\n\npublic record ${NAME}() {\n}\n";
const ANNOTATION: &str = "package ${PACKAGE_NAME};\n\npublic @interface ${NAME} {\n}\n";
const PACKAGE_INFO: &str = "package ${PACKAGE_NAME};\n";

/// The text of a new source file before the user changes it.
pub fn default_file_text(kind: &str) -> Option<&'static str> {
    match kind {
        "class" => Some(CLASS),
        "interface" => Some(INTERFACE),
        "enum" => Some(ENUM),
        "record" => Some(RECORD),
        "annotation" => Some(ANNOTATION),
        "package-info" => Some(PACKAGE_INFO),
        _ => None,
    }
}

/// Every kind of new file the project tree offers, with its text and its package rule.
pub fn file_templates() -> Vec<FileTemplateDefinition> {
    let named = |id: &str, label: &str, text: &str| {
        FileTemplateDefinition::new(id, label, text, package_of)
    };
    vec![
        named("class", "Java Class", CLASS),
        named("interface", "Interface", INTERFACE),
        named("enum", "Enum", ENUM),
        named("record", "Record", RECORD),
        named("annotation", "Annotation", ANNOTATION),
        named("package-info", "package-info", PACKAGE_INFO),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn every_template_has_its_text_and_no_unknown_kind() {
        for definition in file_templates() {
            assert!(
                default_file_text(&definition.id).is_some(),
                "{}",
                definition.id
            );
            assert!(!definition.label.is_empty());
        }
        assert!(default_file_text("unknown").is_none());
    }

    #[test]
    fn the_class_template_knows_its_package_and_name() {
        let root = Path::new("C:/shop");
        let folder = root.join("src/main/java/com/shop");
        let class = file_templates()
            .into_iter()
            .find(|definition| definition.id == "class")
            .unwrap();
        assert_eq!(
            class.render(root, &folder, "Order"),
            "package com.shop;\n\npublic class Order {\n}\n"
        );
        assert_eq!(
            class.render(root, root, "Order"),
            "public class Order {\n}\n"
        );
    }
}
