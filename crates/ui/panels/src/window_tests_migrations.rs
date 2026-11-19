//! T-0248: the migrations of Flyway and Liquibase without a window. The project tree
//! shows their files in the order the tool runs them, the Database window marks a
//! repeated version and the state a history table gives, the New menu writes the next
//! Flyway file, and a Liquibase changelog follows its includes and completes a change in
//! the editor. The language of the test is the real one through `window_language`; the
//! rows of the history table are the answers the test hands back.
//!
//! `cargo test -p panels window_tests_migrations`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};

use lang_api::{MigrationHistoryRows, MigrationKind, MigrationState, MigrationTool};
use project_model::{Dependency, DependencyScope, Module, ModuleDependencies, ProjectModel, Unit};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, PROJECT_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, ActiveMigrations};
use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, scratch, type_text,
    wait_for_signal, with_window,
};

const FLYWAY: &str = "org.flywaydb:flyway-core:10.20.0";
const LIQUIBASE: &str = "org.liquibase:liquibase-core:4.29.0";

fn project_model(root: &Path, coordinates: &str) -> ProjectModel {
    let resources = root.join("src").join("main").join("resources");
    ProjectModel {
        root: root.to_path_buf(),
        tool: "pom".to_owned(),
        resolved: true,
        modules: vec![Module {
            id: "app".to_owned(),
            name: "shop".to_owned(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".to_owned(),
                resource_roots: vec![resources],
                ..Unit::default()
            }],
            ..Module::default()
        }],
        dependencies: vec![ModuleDependencies {
            module: "app".to_owned(),
            scopes: vec![DependencyScope {
                name: "compile".to_owned(),
                roots: vec![Dependency {
                    coordinates: coordinates.to_owned(),
                    ..Dependency::default()
                }],
            }],
            ..ModuleDependencies::default()
        }],
        ..ProjectModel::default()
    }
}

/// The build file of the project: its group is the one of the tool the model names, and
/// `plugins` is the part of the build that runs the tool.
fn build_file(root: &Path, group: &str, plugins: &str) {
    fs::write(
        root.join("pom.xml"),
        format!(
            "<project><modelVersion>4.0.0</modelVersion>\
             <groupId>{group}</groupId><artifactId>shop</artifactId><version>1.0</version>\
             {plugins}</project>"
        ),
    )
    .unwrap();
}

fn resource(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root
        .join("src")
        .join("main")
        .join("resources")
        .join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

/// A project with the build file, the dependency of its tool and the resources the test
/// writes, in the window with the migrations the language reads from its model.
fn open(
    cx: &mut TestAppContext,
    root: &Path,
    group: &str,
    coordinates: &str,
) -> WindowHandle<Workspace> {
    open_with(cx, root, group, coordinates, "")
}

fn open_with(
    cx: &mut TestAppContext,
    root: &Path,
    group: &str,
    coordinates: &str,
    plugins: &str,
) -> WindowHandle<Workspace> {
    build_file(root, group, plugins);
    let model = project_model(root, coordinates);
    let support = window_language::migrations(root, &model);
    cx.update(|cx| cx.set_global(ActiveMigrations(Some(support))));
    let window = open_window(cx, root.to_path_buf());
    draw(cx, window);
    window
}

fn project_panel(cx: &TestAppContext) -> ui_kit::gpui::Entity<crate::project::ProjectPanel> {
    cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

fn database_panel(
    cx: &TestAppContext,
) -> ui_kit::gpui::Entity<crate::database_panel::DatabasePanel> {
    cx.update(|cx| cx.global::<ActiveDatabase>().0.clone())
}

/// Waits until the language has read the migration files into the window.
fn wait_for_files(cx: &mut TestAppContext, count: usize) {
    let panel = database_panel(cx);
    wait_for_signal(cx, "the migration files", |cx| {
        panel.read_with(cx, |panel, _| panel.migrations().rows.len() == count)
    });
}

fn popup_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds("completion-popup").is_some()
}

/// Clicks the row of the open menu with `label`.
fn click_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let index = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .and_then(|entries| entries.iter().position(|entry| entry.label == label))
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("the menu has no row {label}"));
    // The test platform looks its selectors up by a static name.
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
}

#[ui_kit::gpui::test]
fn a_flyway_project_shows_its_files_in_version_order(cx: &mut TestAppContext) {
    let root = scratch("migrations-flyway-tree");
    resource(
        &root,
        "db/migration/V2__owners.sql",
        "CREATE TABLE owners (id INT);",
    );
    resource(&root, "db/migration/V1.10__later.sql", "");
    resource(&root, "db/migration/V1.1__early.sql", "");
    resource(
        &root,
        "db/migration/R__views.sql",
        "CREATE VIEW v AS SELECT 1;",
    );
    resource(&root, "db/migration/V1__init.sql", "");
    resource(&root, "db/migration/V1_init.sql", "");
    let window = open(cx, &root, "org.flywaydb", FLYWAY);
    wait_for_files(cx, 5);
    let panel = project_panel(cx);

    let labels: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .migrations()
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    });
    assert_eq!(
        labels,
        [
            "V1__init.sql",
            "V1.1__early.sql",
            "V1.10__later.sql",
            "V2__owners.sql",
            "R__views.sql",
        ]
    );
    let actions = panel.read_with(cx, |panel, _| panel.migrations().actions.clone());
    assert_eq!(actions.len(), 4);
    assert!(
        actions
            .iter()
            .all(|action| action.goal.is_none() && action.task.is_none())
    );
    assert!(
        actions.iter().all(|action| action.unavailable.as_deref()
            == Some("Flyway is not a plugin of the build of this project")),
        "{actions:?}"
    );

    // The node of the migrations holds the files the tree shows, after the rows of the
    // project; the Scratches folder keeps its own row.
    let rows = panel.read_with(cx, |panel, _| panel.tree().rows().to_vec());
    let node = rows
        .iter()
        .find(|row| row.name == "Migrations" && row.folder)
        .expect("the project tree has the migration node")
        .path
        .clone();
    assert!(rows.iter().any(|row| row.name == "Scratches" && row.folder));
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.toggle(&node, cx))
    });
    let children: Vec<String> = panel.read_with(cx, |panel, _| {
        let rows = panel.tree().rows();
        let start = rows
            .iter()
            .position(|row| row.path == node)
            .expect("the migration node is in the tree");
        rows.iter()
            .skip(start + 1)
            .take_while(|row| row.depth > 1)
            .map(|row| row.name.clone())
            .collect()
    });
    assert_eq!(
        children,
        [
            "V1__init.sql",
            "V1.1__early.sql",
            "V1.10__later.sql",
            "V2__owners.sql",
            "R__views.sql",
        ]
    );
}

#[ui_kit::gpui::test]
fn a_repeated_flyway_version_is_underlined_in_the_database_window(cx: &mut TestAppContext) {
    let root = scratch("migrations-flyway-duplicate");
    resource(&root, "db/migration/V1__init.sql", "");
    resource(&root, "db/migration/V1__owners.sql", "");
    let window = open(cx, &root, "org.flywaydb", FLYWAY);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    wait_for_files(cx, 2);
    let panel = database_panel(cx);
    let issues: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .migrations()
            .issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect()
    });
    assert_eq!(
        issues,
        ["flyway.duplicateVersion", "flyway.duplicateVersion"]
    );
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for index in 0..2 {
        let selector: &'static str =
            Box::leak(format!("database-migration-{index}-issue").into_boxed_str());
        assert!(
            visual.debug_bounds(selector).is_some(),
            "the row {index} of the repeated version is underlined"
        );
    }
}

#[ui_kit::gpui::test]
fn the_new_menu_writes_the_next_flyway_migration(cx: &mut TestAppContext) {
    let root = scratch("migrations-flyway-new");
    resource(&root, "db/migration/V1__init.sql", "");
    resource(&root, "db/migration/V1.2__owners.sql", "");
    let window = open(cx, &root, "org.flywaydb", FLYWAY);
    wait_for_files(cx, 2);
    let panel = project_panel(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&root, cx))
    });
    draw(cx, window);

    press(cx, window, "alt-insert");
    draw(cx, window);
    click_menu_row(cx, window, "New Flyway Migration");
    draw(cx, window);
    type_text(cx, window, "Add Pet Owners");
    press(cx, window, "enter");

    let path = root.join("src/main/resources/db/migration/V1.3__add_pet_owners.sql");
    wait_for_signal(cx, "the new migration", |_| path.is_file());
    assert_eq!(fs::read_to_string(&path).unwrap(), "-- Add Pet Owners\n");
}

#[ui_kit::gpui::test]
fn a_liquibase_project_follows_an_include_and_completes_a_change(cx: &mut TestAppContext) {
    let root = scratch("migrations-liquibase");
    let master = resource(
        &root,
        "db/changelog/db.changelog-master.xml",
        "<databaseChangeLog>\n\
         \x20   <changeSet id=\"1\" author=\"bob\">\n\
         \x20   </changeSet>\n\
         \x20   <include file=\"owners.xml\" relativeToChangelogFile=\"true\"/>\n\
         </databaseChangeLog>\n",
    );
    let owners = resource(
        &root,
        "db/changelog/owners.xml",
        "<databaseChangeLog>\n\
         \x20   <changeSet id=\"2\" author=\"bob\">\n\
         \x20       <createTable tableName=\"owners\"/>\n\
         \x20   </changeSet>\n\
         </databaseChangeLog>\n",
    );
    let window = open(cx, &root, "org.liquibase", LIQUIBASE);
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(window_language::changelogs(&root)))
        })
        .unwrap();
    wait_for_files(cx, 5);
    let panel = database_panel(cx);

    let rows = panel.read_with(cx, |panel, _| panel.migrations().rows.clone());
    let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "db.changelog-master.xml",
            "1:bob",
            "owners.xml",
            "owners.xml",
            "2:bob",
        ]
    );
    let include = rows
        .iter()
        .find(|row| row.kind == MigrationKind::Include)
        .expect("the changelog has its include");
    assert_eq!(include.target.as_deref(), Some(owners.as_path()));
    assert!(
        panel.read_with(cx, |panel, _| panel.migrations().issues.is_empty()),
        "a changelog that names its files has no issue"
    );

    // The caret at the start of the closing tag of the changeSet completes the changes.
    open_path(cx, window, &master);
    let view = editor_of(cx, window, &master);
    view.update(cx, |view, cx| {
        view.go_to(editor_view::Spot::Line { line: 2, column: 4 }, cx)
    });
    type_text(cx, window, "<");
    press(cx, window, "ctrl-space");
    wait_for_signal(cx, "the changes of a changeset", |cx| {
        draw(cx, window);
        popup_shown(cx, window)
    });
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(
        text.contains("createTable tableName=\"\""),
        "the completed change is in the changelog: {text}"
    );
}

#[ui_kit::gpui::test]
fn the_history_of_the_database_marks_the_files_applied_pending_or_changed(cx: &mut TestAppContext) {
    let root = scratch("migrations-history");
    resource(
        &root,
        "db/migration/V1__init.sql",
        "CREATE TABLE owners (id INT);\n",
    );
    resource(
        &root,
        "db/migration/V2__owners.sql",
        "CREATE TABLE vets (id INT);\n",
    );
    let window = open(cx, &root, "org.flywaydb", FLYWAY);
    wait_for_files(cx, 2);
    let panel = database_panel(cx);
    let history = |checksum: &str| MigrationHistoryRows {
        tool: MigrationTool::Flyway,
        columns: vec![
            "version".to_owned(),
            "description".to_owned(),
            "checksum".to_owned(),
            "success".to_owned(),
        ],
        rows: vec![vec![
            "1".to_owned(),
            "init".to_owned(),
            checksum.to_owned(),
            "true".to_owned(),
        ]],
    };

    panel.update(cx, |panel, cx| {
        panel.migration_history_for_test(vec![history("")], cx)
    });
    wait_for_signal(cx, "the state of the applied file", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.migrations().rows[0].state == MigrationState::Applied
        })
    });
    let states: Vec<MigrationState> = panel.read_with(cx, |panel, _| {
        panel
            .migrations()
            .rows
            .iter()
            .map(|row| row.state)
            .collect()
    });
    assert_eq!(states, [MigrationState::Applied, MigrationState::Pending]);

    // Another checksum of the same version: the file was changed after the database ran
    // it, and the window carries the check of that.
    panel.update(cx, |panel, cx| {
        panel.migration_history_for_test(vec![history("7")], cx)
    });
    wait_for_signal(cx, "the changed file", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.migrations().rows[0].state == MigrationState::Changed
        })
    });
    let ids: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .migrations()
            .issues
            .iter()
            .map(|issue| issue.id.clone())
            .collect()
    });
    assert_eq!(ids, ["flyway.changedMigration"]);
    draw(cx, window);
}
