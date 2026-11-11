mod support;

use std::process::Command;

use support::Fixture;

fn check_layers(fixture: &Fixture) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["check-layers", "--root"])
        .arg(&fixture.root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.success(), stderr)
}

fn assert_passes(fixture: &Fixture) {
    let (success, stderr) = check_layers(fixture);
    assert!(success, "expected a pass, got:\n{stderr}");
}

fn assert_fails(fixture: &Fixture, expected: &str) {
    let (success, stderr) = check_layers(fixture);
    assert!(!success, "expected a failure with `{expected}`");
    assert!(
        stderr.contains(expected),
        "expected `{expected}` in:\n{stderr}"
    );
}

#[test]
fn allowed_layers_pass() {
    let fixture = Fixture::new("layers-allowed")
        .krate("crates/foundation/base", &[])
        .krate("crates/services/search", &["crates/foundation/base"])
        .krate("crates/ai/agent-core", &["crates/services/search"])
        .krate("crates/java/java-index", &["crates/foundation/base"])
        .krate("crates/java/java-lang", &["crates/java/java-index"])
        .krate("crates/app/ide", &["crates/java/java-lang"]);
    assert_passes(&fixture);
}

#[test]
fn ai_crate_depending_on_ui_fails() {
    let fixture = Fixture::new("layers-ai-ui")
        .krate("crates/ui/panels", &[])
        .krate("crates/ai/agent-core", &["crates/ui/panels"]);
    assert_fails(&fixture, "agent-core must not depend on panels");
}

#[test]
fn ai_crate_depending_on_app_fails() {
    let fixture = Fixture::new("layers-ai-app")
        .krate("crates/app/ide-mcp", &[])
        .krate("crates/ai/agent-core", &["crates/app/ide-mcp"]);
    assert_fails(&fixture, "agent-core must not depend on ide-mcp");
}

#[test]
fn neutral_crate_depending_on_java_fails() {
    let fixture = Fixture::new("layers-neutral-java")
        .krate("crates/java/java-index", &[])
        .krate("crates/services/search", &["crates/java/java-index"]);
    assert_fails(&fixture, "search must not depend on java-index");
}

#[test]
fn ide_reaches_java_only_through_the_facade() {
    let fixture = Fixture::new("layers-ide-java")
        .krate("crates/java/java-index", &[])
        .krate("crates/app/ide", &["crates/java/java-index"]);
    assert_fails(&fixture, "ide must not depend on java-index");
}

#[test]
fn only_ui_kit_depends_on_gpui() {
    let fixture = Fixture::new("layers-gpui-allowed")
        .krate("vendor/gpui-kit", &[])
        .krate("crates/ui/ui-kit", &["vendor/gpui-kit"]);
    assert_passes(&fixture);

    let fixture = Fixture::new("layers-gpui-denied")
        .krate("vendor/gpui-kit", &[])
        .krate("crates/ui/panels", &["vendor/gpui-kit"]);
    assert_fails(&fixture, "panels must not depend on gpui-kit");
}

#[test]
fn display_map_uses_sum_tree_but_not_gpui() {
    let fixture = Fixture::new("layers-sum-tree")
        .krate("vendor/gpui-pre-sum-tree", &[])
        .krate("crates/editor/display-map", &["vendor/gpui-pre-sum-tree"]);
    assert_passes(&fixture);

    let fixture = Fixture::new("layers-gpui-pre")
        .krate("vendor/gpui-pre", &[])
        .krate("crates/editor/display-map", &["vendor/gpui-pre"]);
    assert_fails(&fixture, "display-map must not depend on gpui-pre");
}

#[test]
fn editor_core_must_not_reach_ui_even_indirectly() {
    let fixture = Fixture::new("layers-ui-transitive")
        .krate("crates/ui/ui-kit", &[])
        .krate("crates/foundation/base", &["crates/ui/ui-kit"])
        .krate("crates/editor/text-core", &["crates/foundation/base"]);
    assert_fails(&fixture, "text-core must not reach ui-kit");
}

#[test]
fn services_must_not_reach_ui() {
    let fixture = Fixture::new("layers-services-ui")
        .krate("crates/ui/ui-kit", &[])
        .krate("crates/services/search", &["crates/ui/ui-kit"]);
    assert_fails(&fixture, "search must not reach ui-kit");
}

#[test]
fn win32_dependency_outside_allowed_crates_fails() {
    let fixture = Fixture::new("layers-win32-allowed")
        .krate("vendor/windows-sys", &[])
        .krate("crates/foundation/platform", &["vendor/windows-sys"]);
    assert_passes(&fixture);

    let fixture = Fixture::new("layers-win32-denied")
        .krate("vendor/windows-sys", &[])
        .krate("crates/services/search", &["vendor/windows-sys"]);
    assert_fails(&fixture, "search must not depend on windows-sys");
}

#[test]
fn foreign_abi_outside_win32_crates_fails() {
    let sources = [
        "unsafe extern \"system\" { fn GetTickCount() -> u32; }\n",
        "#[link(name = \"kernel32\")]\nunsafe extern \"C\" { fn GetTickCount() -> u32; }\n",
        "#[cfg_attr(windows, link(name = \"kernel32\", kind = \"raw-dylib\"))]\n\
         unsafe extern \"C\" { fn GetTickCount() -> u32; }\n",
    ];
    for (index, source) in sources.into_iter().enumerate() {
        let fixture = Fixture::new(&format!("layers-abi-{index}"))
            .krate("crates/services/search", &[])
            .source("crates/services/search/src/lib.rs", source);
        assert_fails(&fixture, "Win32 lives in platform");
    }
}

#[test]
fn foreign_abi_is_fine_in_win32_crates() {
    let fixture = Fixture::new("layers-abi-allowed")
        .krate("crates/foundation/platform", &[])
        .source(
            "crates/foundation/platform/src/lib.rs",
            "#[link(name = \"kernel32\")]\nunsafe extern \"system\" { fn GetTickCount() -> u32; }\n",
        );
    assert_passes(&fixture);
}

#[test]
fn forbidden_word_in_neutral_crate_fails() {
    let fixture = Fixture::new("layers-word")
        .krate("crates/services/search", &[])
        .source(
            "crates/services/search/src/deep/inner.rs",
            "pub struct MavenRunner;\n",
        );
    assert_fails(&fixture, "forbidden word `maven`");
}

#[test]
fn forbidden_words_are_fine_in_java_crates_and_the_ide() {
    let fixture = Fixture::new("layers-word-allowed")
        .krate("crates/java/java-project", &[])
        .source("crates/java/java-project/src/lib.rs", "pub struct Maven;\n")
        .krate("crates/app/ide", &[])
        .source("crates/app/ide/src/lib.rs", "// picks java-lang\n");
    assert_passes(&fixture);
}

#[test]
fn directory_literal_in_neutral_crate_fails() {
    let fixture = Fixture::new("layers-literal")
        .krate("crates/services/runner", &[])
        .source(
            "crates/services/runner/src/lib.rs",
            "pub const OUTPUT: &str = \"target/classes\";\n",
        );
    assert_fails(&fixture, "forbidden word `target`");
}
