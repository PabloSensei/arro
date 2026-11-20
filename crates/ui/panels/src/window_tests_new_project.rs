//! New Project in a window without a window: the generators of the language, the form
//! of the wizard, the folder it writes and the server a Spring Boot project comes from.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use project_model::scaffold::Generator;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, gpui};

use crate::start::StartScreen;
use crate::window_tests::{click, draw, scratch, wait_for_signal};

/// A screen with the generators of the application, already in the New Project mode.
pub(crate) fn open(
    cx: &mut TestAppContext,
    name: &str,
    generators: Vec<Arc<dyn Generator>>,
) -> WindowHandle<StartScreen> {
    let state = scratch(&format!("new-project-state-{name}"));
    let screen = cx.add_window(|window, cx| {
        cx.set_global(Theme::dark());
        let mut screen = StartScreen::new_for_test(Vec::new(), state, Vec::new(), window, cx);
        screen.set_generators_for_test(generators);
        screen.keep_window_for_test();
        screen
    });
    cx.run_until_parked();
    draw(cx, screen);
    click(cx, screen, "start-new");
    draw(cx, screen);
    screen
}

/// The generators of the application, with the wrapper of the build tool off: a test
/// must not run the build tool of the machine.
pub(crate) fn generators(name: &str) -> Vec<Arc<dyn Generator>> {
    let options = window_language::NewProjectOptions {
        wrapper: false,
        ..Default::default()
    };
    window_language::project_generators(&scratch(&format!("new-project-cache-{name}")), &options)
}

/// The same with the metadata of a server of the test.
fn generators_of_server(name: &str, server: String) -> Vec<Arc<dyn Generator>> {
    let options = window_language::NewProjectOptions {
        wrapper: false,
        initializr: server,
    };
    window_language::project_generators(&scratch(&format!("new-project-cache-{name}")), &options)
}

/// Types into the field the wizard has active.
fn type_into(cx: &mut TestAppContext, screen: WindowHandle<StartScreen>, text: &str) {
    cx.simulate_input(screen.into(), text);
    cx.run_until_parked();
}

fn set_fields(
    cx: &mut TestAppContext,
    screen: WindowHandle<StartScreen>,
    name: &str,
    parent: &Path,
) {
    screen
        .update(cx, |screen, _, cx| {
            screen.set_new_project_fields_for_test(name, &parent.display().to_string());
            cx.notify();
        })
        .unwrap();
    draw(cx, screen);
}

#[ui_kit::gpui::test]
fn the_wizard_writes_a_project_and_opens_it(cx: &mut TestAppContext) {
    let screen = open(cx, "plain", generators("plain"));
    let parent = scratch("new-project-plain");
    let folder = parent.join("demo");
    set_fields(cx, screen, "demo", &parent);

    click(cx, screen, "new-project-create");
    wait_for_signal(cx, "the new project", |_| folder.join("pom.xml").is_file());

    let pom = fs::read_to_string(folder.join("pom.xml")).unwrap();
    let home = screen
        .read_with(cx, |screen, _| screen.wizard_for_test().value("jdk"))
        .unwrap()
        .unwrap_or_default();
    let release = window_language::jdk_feature(Path::new(&home)).unwrap_or(21);
    assert_eq!(
        window_language::build_release(&folder),
        Some(release),
        "the release of the chosen JDK: {pom}"
    );
    assert!(pom.contains("<artifactId>demo</artifactId>"), "{pom}");
    assert_eq!(
        window_language::sample_sources(&folder).len(),
        1,
        "the sample code of the project"
    );
    wait_for_signal(cx, "the opened project", |cx| {
        screen
            .read_with(cx, |screen, _| screen.message_for_test())
            .unwrap()
            .unwrap_or_default()
            .starts_with("Would open")
    });
}

#[ui_kit::gpui::test]
fn the_wizard_writes_a_gradle_project_in_kotlin(cx: &mut TestAppContext) {
    let screen = open(cx, "gradle", generators("gradle"));
    let parent = scratch("new-project-gradle");
    let folder = parent.join("shop");
    click(cx, screen, "new-project-choice-build-gradle");
    draw(cx, screen);
    click(cx, screen, "new-project-choice-dsl-kotlin");
    draw(cx, screen);
    set_fields(cx, screen, "shop", &parent);

    click(cx, screen, "new-project-create");
    wait_for_signal(cx, "the new Gradle project", |_| {
        folder.join("build.gradle.kts").is_file()
    });

    let script = fs::read_to_string(folder.join("build.gradle.kts")).unwrap();
    assert!(script.contains("testImplementation"), "{script}");
    assert!(window_language::build_release(&folder).is_some());
    assert!(folder.join("settings.gradle.kts").is_file());
    assert!(
        !folder.join("build.gradle").exists(),
        "a Kotlin project has no Groovy script"
    );
}

#[ui_kit::gpui::test]
fn the_form_refuses_an_empty_name_and_a_folder_with_files(cx: &mut TestAppContext) {
    let screen = open(cx, "errors", generators("errors"));
    assert!(
        !screen
            .read_with(cx, |screen, _| screen.wizard_for_test().can_create())
            .unwrap(),
        "a project without a name cannot be written"
    );
    click(cx, screen, "new-project-create");
    cx.run_until_parked();
    let error = screen
        .read_with(cx, |screen, _| screen.wizard_for_test().error_text())
        .unwrap();
    assert_eq!(error.as_deref(), Some("The name is empty"));

    let parent = scratch("new-project-errors");
    let folder = parent.join("demo");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("keep.txt"), "mine").unwrap();
    set_fields(cx, screen, "demo", &parent);

    let error = screen
        .read_with(cx, |screen, _| screen.wizard_for_test().error_text())
        .unwrap()
        .unwrap_or_default();
    assert!(error.contains("not empty"), "{error}");
    assert!(
        !screen
            .read_with(cx, |screen, _| screen.wizard_for_test().can_create())
            .unwrap(),
        "the Create button is inactive over a folder with files"
    );
    click(cx, screen, "new-project-create");
    cx.run_until_parked();
    assert!(!folder.join("pom.xml").exists());
    assert_eq!(fs::read_to_string(folder.join("keep.txt")).unwrap(), "mine");
}

#[ui_kit::gpui::test]
fn the_spring_wizard_searches_a_dependency_and_unpacks_the_archive(cx: &mut TestAppContext) {
    let server = FakeInitializr::start(
        window_language::initializr_metadata_fixture(),
        window_language::initializr_archive_fixture(),
    );
    let screen = open(cx, "spring", generators_of_server("spring", server.url()));
    click(cx, screen, "new-project-generator-1");
    draw(cx, screen);
    wait_for_signal(cx, "the metadata of the server", |cx| {
        screen
            .read_with(cx, |screen, _| {
                let wizard = screen.wizard_for_test();
                !wizard.loading() && !wizard.choices("dependencies").is_empty()
            })
            .unwrap()
    });
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.wizard_for_test().choices("boot"))
            .unwrap(),
        ["4.0.0", "3.5.6"],
        "the versions of the server fill the field"
    );

    // The form is long, so the keys reach the search of the dependencies: Name,
    // Location and the four text fields of the generator before it.
    cx.simulate_keystrokes(screen.into(), "tab tab tab tab tab tab");
    cx.run_until_parked();
    type_into(cx, screen, "web");
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen
                .wizard_for_test()
                .search_text("dependencies"))
            .unwrap(),
        Some("web".to_owned()),
        "the query narrows the list"
    );
    cx.simulate_keystrokes(screen.into(), "enter");
    cx.run_until_parked();
    draw(cx, screen);
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen
                .wizard_for_test()
                .chosen("dependencies"))
            .unwrap(),
        ["web"]
    );

    let parent = scratch("new-project-spring");
    let folder = parent.join("demo");
    set_fields(cx, screen, "demo", &parent);
    click(cx, screen, "new-project-create");
    cx.run_until_parked();
    wait_for_signal(cx, "the new Spring Boot project", |_| {
        folder.join("pom.xml").is_file()
    });

    assert_eq!(
        fs::read_to_string(folder.join("pom.xml")).unwrap(),
        "<project/>"
    );
    assert!(!window_language::sample_sources(&folder).is_empty());
}

#[ui_kit::gpui::test]
fn a_server_that_does_not_answer_is_a_message_and_not_a_crash(cx: &mut TestAppContext) {
    let screen = open(cx, "offline", generators_of_server("offline", closed_url()));
    click(cx, screen, "new-project-generator-1");
    draw(cx, screen);
    wait_for_signal(cx, "the message of the offline server", |cx| {
        screen
            .read_with(cx, |screen, _| screen.wizard_for_test().notice_text())
            .unwrap()
            .is_some_and(|text| text.contains("Cannot reach"))
    });

    let parent = scratch("new-project-offline");
    set_fields(cx, screen, "demo", &parent);
    click(cx, screen, "new-project-create");
    wait_for_signal(cx, "the message of the failed download", |cx| {
        screen
            .read_with(cx, |screen, _| screen.message_for_test())
            .unwrap()
            .unwrap_or_default()
            .contains("Cannot reach")
    });
    assert!(!parent.join("demo").join("pom.xml").exists());
}

#[ui_kit::gpui::test]
fn a_new_project_can_start_a_repository(cx: &mut TestAppContext) {
    if vcs::GitRunner::from_env().is_none() {
        return;
    }
    let screen = open(cx, "git", generators("git"));
    click(cx, screen, "new-project-generator-2");
    draw(cx, screen);
    let parent = scratch("new-project-git");
    let folder = parent.join("blank");
    set_fields(cx, screen, "blank", &parent);
    click(cx, screen, "new-project-git");
    draw(cx, screen);

    click(cx, screen, "new-project-create");
    wait_for_signal(cx, "the repository", |_| folder.join(".git").is_dir());

    assert!(folder.join(".git").is_dir());
    assert_eq!(
        screen
            .read_with(cx, |screen, _| screen.wizard_for_test().value("sample"))
            .unwrap(),
        None,
        "the empty project has no fields"
    );
}

#[ui_kit::gpui::test]
fn file_new_project_opens_the_wizard_in_a_window_of_its_own(cx: &mut TestAppContext) {
    crate::set_project_generators(Arc::new(Vec::new));
    let (_, window) = crate::window_tests::open_sample_window(cx, "new-project-menu");
    let before = cx.update(|cx| cx.windows().len());

    // File > New > Project...
    let new_row = window
        .update(cx, |workspace, _, cx| {
            workspace.main_menu(cx)[0]
                .submenu
                .iter()
                .find(|entry| entry.label.as_ref() == "New")
                .map(|entry| entry.submenu.clone())
        })
        .unwrap()
        .expect("the File menu has a New submenu");
    assert_eq!(new_row[0].label.as_ref(), "Project...");

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("file.new_project", window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        before + 1,
        "the wizard opens in a window of its own"
    );
}

/// A port with nothing behind it: the server of a test that is offline.
fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A server of the Spring Initializr interface: the metadata for `/`, the archive for
/// `/starter.zip`. One connection answers one request; the client of a test opens a new
/// one for the archive. It stops with the test process.
struct FakeInitializr {
    port: u16,
}

impl FakeInitializr {
    fn start(metadata: &'static str, archive: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                // The request line may arrive after the headers, so read until the empty
                // line and answer what it asked for.
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                let request = String::from_utf8_lossy(&request).to_string();
                let (kind, body): (&str, &[u8]) = if request.starts_with("GET /starter.zip") {
                    ("application/zip", &archive)
                } else {
                    ("application/json", metadata.as_bytes())
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        Self { port }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}
