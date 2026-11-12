//! The pages of Settings that point the window at the tools outside it: the build tools,
//! Git and the terminal. Every test here proves that a value of the page reaches the
//! program that runs, without a real window.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use editor_view::Languages;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Launch, Layout, Settings, Workspace};

use crate::window_tests::{press, project_setup, scratch, wait_for_signal};
use crate::window_tests_git::{git_state, repository_with_origin, wait_for};
use crate::{AgentOptions, LanguageServer, SearchSetup};

/// The same window as `window_tests::open_window_with`, with the settings the test needs
/// before the tool windows read them.
fn open_with_settings(
    cx: &mut TestAppContext,
    root: PathBuf,
    settings: Settings,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(settings);
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: workspace::FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        crate::install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        crate::connect(&mut workspace, server, cx);
        workspace
    })
}

/// The language names its build tools; the Settings pages show them.
fn install_names() {
    let (first, second) = window_language::build_tool_names();
    crate::settings_catalog::set_build_tool_names(first, second);
}

#[test]
fn every_new_setting_is_found_by_the_search_and_has_a_writer() {
    install_names();
    let (first, second) = window_language::build_tool_names();
    assert_eq!(
        crate::settings_catalog::SettingsPage::BuildToolsFirst.title(),
        first
    );
    assert_eq!(
        crate::settings_catalog::SettingsPage::BuildToolsSecond.title(),
        second
    );

    let new_pages = [
        crate::settings_catalog::SettingsPage::BuildToolsFirst,
        crate::settings_catalog::SettingsPage::BuildToolsSecond,
        crate::settings_catalog::SettingsPage::Git,
        crate::settings_catalog::SettingsPage::Terminal,
    ];
    let mut settings = Settings::default();
    let mut seen = 0;
    for setting in crate::settings_catalog::all() {
        if !new_pages.contains(&setting.page) {
            continue;
        }
        seen += 1;
        assert!(
            crate::settings_catalog::search(setting.title, 200)
                .iter()
                .any(|found| found.id == setting.id),
            "{} is found by its name",
            setting.id
        );
        match setting.kind {
            crate::settings_catalog::SettingKind::Toggle => {
                let before = crate::settings_catalog::value(&settings, setting.id);
                assert!(before.is_some(), "{} is a switch", setting.id);
                assert!(crate::settings_catalog::toggle(&mut settings, setting.id));
                assert_ne!(
                    crate::settings_catalog::value(&settings, setting.id),
                    before,
                    "{} changed",
                    setting.id
                );
            }
            crate::settings_catalog::SettingKind::Text => {
                let text = format!("value of {}", setting.id);
                assert!(
                    crate::settings_catalog::set_text(&mut settings, setting.id, &text),
                    "{} takes text",
                    setting.id
                );
                assert_eq!(
                    crate::settings_catalog::text(&settings, setting.id).as_deref(),
                    Some(text.as_str()),
                    "{} keeps the text",
                    setting.id
                );
            }
            crate::settings_catalog::SettingKind::Choice => {
                let before = crate::settings_catalog::choice_label(&settings, setting.id);
                assert!(before.is_some(), "{} has a label", setting.id);
                assert!(crate::settings_catalog::next_choice(
                    &mut settings,
                    setting.id
                ));
                assert_ne!(
                    crate::settings_catalog::choice_label(&settings, setting.id),
                    before
                );
            }
            crate::settings_catalog::SettingKind::Number => {
                panic!("{} is not a number", setting.id)
            }
        }
    }
    assert!(seen >= 15, "the four pages have their settings: {seen}");

    let dir = scratch("settings-tools-json");
    let file = dir.join("settings.json");
    settings.save(&file).unwrap();
    assert_eq!(
        Settings::load(&file),
        settings,
        "every new value survives the file"
    );
}

/// The page of the first build tool, as the application builds it for the language.
fn first_tool_settings() -> Settings {
    install_names();
    let mut settings = Settings::default();
    assert!(crate::settings_catalog::toggle(
        &mut settings,
        "build_tool_first_offline"
    ));
    assert!(crate::settings_catalog::toggle(
        &mut settings,
        "build_tool_first_update_snapshots"
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_first_settings_file",
        r"C:\tools\settings.xml"
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_first_vm_options",
        "-Xmx2g"
    ));
    settings
}

#[test]
fn work_offline_reaches_the_build_tool_command_and_the_server_settings() {
    let settings = first_tool_settings();
    let root = scratch("settings-first-tool-offline");
    fs::write(root.join("mvnw.cmd"), "@echo off\r\n").unwrap();
    let jdk = window_language::Jdk {
        home: root.join("jdk"),
        version: "17.0.1".to_owned(),
        feature: 17,
    };
    let spec =
        window_language::first_build_tool_command(&root, &settings.build_tools, jdk, "compile");
    let args: Vec<String> = spec
        .arguments()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args.iter().any(|arg| arg == "-o"), "{args:?}");
    assert!(args.iter().any(|arg| arg == "-U"), "{args:?}");
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "-s" && pair[1] == r"C:\tools\settings.xml"),
        "{args:?}"
    );
    let opts = window_language::first_tool_vm_options(&spec);
    assert_eq!(
        opts,
        Some("-Xmx2g".to_owned()),
        "the VM options of the page reach the JVM of the build tool"
    );

    let config =
        window_language::config_with(&root, &root.join("state"), &settings.build_tools.clone());
    assert!(config.offline, "the server imports offline");
    assert_eq!(
        window_language::server_first_tool_settings(&config).as_deref(),
        Some(r"C:\tools\settings.xml")
    );
    assert!(window_language::server_first_tool_offline(&config));
    assert_eq!(
        window_language::server_first_tool_settings_setting(&config).as_deref(),
        Some(r"C:\tools\settings.xml".replace('\\', "/").as_str())
    );
}

#[test]
fn the_second_build_tool_takes_its_distribution_and_user_home() {
    install_names();
    let root = scratch("settings-second-tool-page");
    window_language::write_second_tool_project(&root);
    let distribution = root.join("distribution");
    let user_home = root.join("user-home");
    let jvm = root.join("jvm");
    fs::create_dir_all(&distribution).unwrap();
    fs::create_dir_all(&user_home).unwrap();
    window_language::write_fake_jdk(&jvm);
    let mut settings = Settings::default();
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_home",
        &distribution.display().to_string()
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_user_home",
        &user_home.display().to_string()
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_runtime_home",
        &jvm.display().to_string()
    ));
    assert!(crate::settings_catalog::toggle(
        &mut settings,
        "build_tool_second_offline"
    ));

    let config = window_language::config_with(&root, &root.join("state"), &settings.build_tools);
    assert_eq!(
        window_language::server_second_tool_home(&config).as_deref(),
        Some(distribution.display().to_string().as_str())
    );
    assert_eq!(
        window_language::server_second_tool_user_home(&config).as_deref(),
        Some(user_home.display().to_string().as_str())
    );
    assert_eq!(
        window_language::server_second_tool_jvm(&config).as_deref(),
        Some(jvm.display().to_string().as_str())
    );
    assert!(window_language::server_second_tool_offline(&config));
    assert!(
        !window_language::server_second_tool_wrapper(&config),
        "a named distribution replaces the wrapper"
    );
    assert_eq!(
        window_language::server_second_tool_home_setting(&config).as_deref(),
        Some(
            distribution
                .display()
                .to_string()
                .replace('\\', "/")
                .as_str()
        )
    );
}

#[test]
fn the_second_build_tool_command_takes_the_offline_flag_and_the_user_home() {
    install_names();
    let root = scratch("settings-second-tool-command");
    window_language::write_second_tool_project(&root);
    let distribution = root.join("distribution");
    let user_home = root.join("user-home");
    let jvm = root.join("jvm");
    window_language::write_fake_distribution(&distribution);
    fs::create_dir_all(&user_home).unwrap();
    window_language::write_fake_jdk(&jvm);
    let mut settings = Settings::default();
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_home",
        &distribution.display().to_string()
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_user_home",
        &user_home.display().to_string()
    ));
    assert!(crate::settings_catalog::set_text(
        &mut settings,
        "build_tool_second_runtime_home",
        &jvm.display().to_string()
    ));
    assert!(crate::settings_catalog::toggle(
        &mut settings,
        "build_tool_second_offline"
    ));

    let spec = window_language::second_build_tool_command(&root, &settings.build_tools, "classes");
    let args: Vec<String> = spec
        .arguments()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args.iter().any(|arg| arg == "--offline"), "{args:?}");
    assert_eq!(
        window_language::second_tool_user_home(&spec),
        Some(user_home.display().to_string())
    );
}

#[ui_kit::gpui::test]
fn the_git_path_of_the_settings_runs_the_git_of_the_window(cx: &mut TestAppContext) {
    install_names();
    let root = scratch("settings-git-path");
    let log = root.join("git-arguments.txt");
    let script = root.join("fake-git.cmd");
    fs::write(
        &script,
        format!(
            "@echo off\r\necho %*>>\"{}\"\r\necho {}\r\n",
            log.display(),
            root.display()
        ),
    )
    .unwrap();
    let settings = Settings {
        git_path: script.display().to_string(),
        ..Settings::default()
    };
    let _window = open_with_settings(cx, root.clone(), settings);
    cx.run_until_parked();

    wait_for_signal(cx, "the stand-in Git to answer", |_| {
        fs::read_to_string(&log).is_ok_and(|text| text.contains("rev-parse --show-toplevel"))
    });
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.contains("--no-optional-locks"), "{text}");
    assert!(text.contains("rev-parse --show-toplevel"), "{text}");
}

#[ui_kit::gpui::test]
fn update_project_rebases_when_the_page_says_rebase(cx: &mut TestAppContext) {
    install_names();
    let (root, bare, git) = repository_with_origin("settings-git-rebase");
    let settings = Settings {
        git_update_method: workspace::GitUpdateMethod::Rebase,
        git_path: String::new(),
        ..Settings::default()
    };
    let window = open_with_settings(cx, root.clone(), settings);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for(cx, "the repository of the window", |cx| {
        state.read_with(cx, |state, _| state.repo().is_some())
    });

    let other = scratch("settings-git-rebase-other");
    git.run(
        &scratch("settings-git-rebase-clone"),
        ["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
    )
    .unwrap();
    git.run(&other, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(&other, ["config", "user.name", "Other"]).unwrap();
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    git.run(&other, ["add", "remote.txt"]).unwrap();
    git.run(&other, ["commit", "-m", "remote change"]).unwrap();
    git.run(&other, ["push"]).unwrap();
    fs::write(root.join("local.txt"), "local\n").unwrap();
    git.run(&root, ["add", "local.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "local change"]).unwrap();

    press(cx, window, "ctrl-t");
    wait_for(cx, "the rebased branch", |cx| {
        state.read_with(cx, |state, _| {
            state.repo().is_some_and(|repo| {
                repo.log(1, 0)
                    .is_ok_and(|log| log.first().is_some_and(|entry| entry.parents.len() == 1))
            })
        })
    });
    let remote = fs::read_to_string(root.join("remote.txt")).unwrap();
    let local = fs::read_to_string(root.join("local.txt")).unwrap();
    assert_eq!(remote.replace("\r\n", "\n"), "remote\n");
    assert_eq!(local.replace("\r\n", "\n"), "local\n");
}

#[ui_kit::gpui::test]
fn the_shell_of_the_terminal_page_starts_the_new_tab_in_its_folder(cx: &mut TestAppContext) {
    install_names();
    let project = scratch("settings-terminal");
    let start = scratch("settings-terminal-start");
    let marker = project.join("terminal-started.txt");
    let script = project.join("fake-shell.cmd");
    fs::write(
        &script,
        format!(
            "@echo off\r\ncd >>\"{}\"\r\necho %SETTINGS_TERMINAL_TEST%>>\"{}\"\r\n",
            marker.display(),
            marker.display()
        ),
    )
    .unwrap();
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
    let shell = format!("\"{comspec}\" /d /c \"{}\"", script.display());
    let settings = Settings {
        terminal_shell: shell,
        terminal_start_directory: start.display().to_string(),
        terminal_environment: "SETTINGS_TERMINAL_TEST=from-the-page".to_owned(),
        ..Settings::default()
    };
    let _window = open_with_settings(cx, project.clone(), settings);

    let starting = start.display().to_string().to_lowercase();
    wait_for_signal(cx, "the stand-in shell", |_| {
        fs::read_to_string(&marker).is_ok_and(|text| {
            text.to_lowercase().contains(&starting) && text.contains("from-the-page")
        })
    });
    let text = fs::read_to_string(&marker).unwrap();
    assert!(
        text.to_lowercase().contains(&starting),
        "the shell starts in {start:?}: {text}"
    );
    assert!(text.contains("from-the-page"), "{text}");
}
