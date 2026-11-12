//! The exact `.editorconfig` of the PetClinic corpus, as `text-core` reads it. The
//! parser lives in a neutral crate, so the sample with the words of this project stays
//! here, in the Java side of the workspace.

use std::fs;
use std::path::{Path, PathBuf};

use text_core::{Charset, EndOfLine, IndentSize, IndentStyle, for_file};

const PETCLINIC: &str = "\
# top-most EditorConfig file
root = true

[*]
charset = utf-8
end_of_line = lf
insert_final_newline = true
indent_style = space

[*.{java,xml}]
indent_size = 4
trim_trailing_whitespace = true
indent_style = tab
tab_width = 4

[{pom,wro}.xml]
indent_size = 2
indent_style = space

[*.{html,sql,less}]
indent_size = 2

[*.gradle]
indent_size = 2
";

fn scratch(name: &str) -> PathBuf {
    test_support::scratch(&format!("java-editorconfig-{name}"))
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn petclinic_java_files_take_tabs_and_its_pom_two_spaces() {
    let root = scratch("petclinic");
    write(&root, ".editorconfig", PETCLINIC);

    let source = write(&root, "src/main/java/A.java", "class A {}\n");
    let properties = for_file(&source);
    assert_eq!(properties.indent_style, Some(IndentStyle::Tab));
    assert_eq!(properties.indent_size, Some(IndentSize::Number(4)));
    assert_eq!(properties.tab_width, Some(4));
    assert_eq!(properties.indentation(), Some((true, 4, 4)));
    assert_eq!(properties.charset, Some(Charset::Utf8));
    assert_eq!(properties.end_of_line, Some(EndOfLine::Lf));
    assert_eq!(properties.insert_final_newline, Some(true));
    assert_eq!(properties.trim_trailing_whitespace, Some(true));

    let pom = write(&root, "pom.xml", "<project/>\n");
    let properties = for_file(&pom);
    assert_eq!(properties.indent_style, Some(IndentStyle::Space));
    assert_eq!(properties.indent_size, Some(IndentSize::Number(2)));
    assert_eq!(properties.indentation(), Some((false, 2, 4)));

    let gradle = write(&root, "build.gradle", "");
    let properties = for_file(&gradle);
    assert_eq!(properties.indent_style, Some(IndentStyle::Space));
    assert_eq!(properties.indent_size, Some(IndentSize::Number(2)));
    assert_eq!(properties.indentation(), Some((false, 2, 2)));
}
