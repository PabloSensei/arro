//! The console helper on a real JDK: fragments, values, output, a compile error with a
//! position, an exception, a stopped loop, completion and the classpath of a module.
//!
//! The helper jar comes from
//! `cargo run -q -p xtask -- build-helpers --out target/helpers` and `IDE_HELPERS_DIR`
//! names that folder. `IDE_CORPUS` names the JDKs when `JAVA_HOME` is not set.
//!
//! `cargo test -p java-lang --test jshell -- --ignored --test-threads=1`

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use java_project::Jdk;
use repl::{Command, CompletionItem, Event, FragmentKind, Session};

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

fn jdk() -> Jdk {
    if let Some(jdk) = Jdk::at(&corpus().join("jdk/21")) {
        return jdk;
    }
    if let Some(jdk) = env::var_os("JAVA_HOME").and_then(|home| Jdk::at(Path::new(&home))) {
        return jdk;
    }
    panic!("no JDK 21 in the corpus and no JAVA_HOME")
}

fn helpers_dir() -> PathBuf {
    if let Some(path) = env::var_os("IDE_HELPERS_DIR") {
        return PathBuf::from(path);
    }
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../target")));
    target.join("helpers")
}

fn helper_jar() -> PathBuf {
    let path = helpers_dir().join("ide-jshell-helper.jar");
    assert!(
        path.is_file(),
        "no helper at {}: run cargo run -q -p xtask -- build-helpers --out target/helpers",
        path.display()
    );
    path
}

fn start(classpath: &Path) -> Session {
    let launch = repl::Launch::new(jdk().java())
        .arg("-cp")
        .arg(helper_jar())
        .arg("ide.jshell.Helper")
        .arg("--classpath")
        .arg(classpath);
    let session = Session::start(launch).expect("the helper starts");
    let Event::Ready { runtime, .. } = wait(&session, |event| matches!(event, Event::Ready { .. }))
    else {
        panic!("the helper did not say it is ready");
    };
    assert!(
        runtime.starts_with("21."),
        "the console runs on JDK 21 of the corpus, not {runtime}"
    );
    session
}

fn wait(session: &Session, found: impl Fn(&Event) -> bool) -> Event {
    let started = Instant::now();
    loop {
        let event = session
            .recv_timeout(Duration::from_secs(120))
            .expect("the helper answers");
        if found(&event) {
            return event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the helper did not send the answer"
        );
    }
}

struct Answer {
    fragments: Vec<Fragment>,
    output: String,
    cancelled: bool,
}

#[derive(Debug)]
struct Fragment {
    kind: FragmentKind,
    name: String,
    value: Option<String>,
    status: String,
    signature: Option<String>,
    diagnostics: Vec<repl::Diagnostic>,
    exception: Option<repl::Exception>,
}

/// Sends one eval and collects its fragments, its output and its cancellation.
fn eval(session: &Session, id: u64, text: &str) -> Answer {
    session
        .send(Command::Eval {
            id,
            text: text.to_owned(),
        })
        .expect("the eval request is written");
    let mut answer = Answer {
        fragments: Vec::new(),
        output: String::new(),
        cancelled: false,
    };
    loop {
        let event = session
            .recv_timeout(Duration::from_secs(120))
            .expect("the helper answers");
        match event {
            Event::Fragment {
                id: found,
                kind,
                name,
                value,
                status,
                signature,
                diagnostics,
                exception,
                ..
            } if found == id => answer.fragments.push(Fragment {
                kind,
                name,
                value,
                status,
                signature,
                diagnostics,
                exception,
            }),
            Event::Output {
                id: found, text, ..
            } if found == id => answer.output.push_str(&text),
            Event::Done {
                id: found,
                cancelled,
            } if found == id => {
                answer.cancelled = cancelled;
                return answer;
            }
            Event::Error { id: found, message } if found == id => panic!("the helper: {message}"),
            _ => {}
        }
    }
}

#[test]
#[ignore = "needs JDK 21, the corpus and the helper jar"]
fn the_console_runs_fragments_with_the_classpath_of_a_project() {
    let project = test_support::TempDir::new("jshell-project");
    let root = project.path().join("project");
    let source = root.join("src/demo/Greeter.java");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        &source,
        "package demo;\npublic class Greeter { public String hi() { return \"hi\"; } }\n",
    )
    .unwrap();
    let classes = project.path().join("classes");
    fs::create_dir_all(&classes).unwrap();
    let javac = jdk()
        .home
        .join("bin")
        .join(if cfg!(windows) { "javac.exe" } else { "javac" });
    let compiled = std::process::Command::new(javac)
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .status()
        .expect("javac runs");
    assert!(compiled.success(), "the small project compiles");
    let cache = project.path().join("cache");
    let classpath = java_exec::classpath_jar(&cache, &root, &[classes]).expect("a classpath jar");

    let session = start(&classpath);

    let answer = eval(&session, 1, "int x = 2; x * 21");
    assert_eq!(answer.fragments.len(), 2);
    assert_eq!(answer.fragments[0].kind, FragmentKind::Variable);
    assert_eq!(answer.fragments[0].name, "x");
    assert_eq!(answer.fragments[0].value.as_deref(), Some("2"));
    assert_eq!(answer.fragments[1].kind, FragmentKind::Expression);
    assert_eq!(answer.fragments[1].value.as_deref(), Some("42"));

    let answer = eval(&session, 2, "void f(int n) { System.out.println(n); }");
    assert_eq!(answer.fragments[0].kind, FragmentKind::Method);
    assert_eq!(answer.fragments[0].signature.as_deref(), Some("f(int)"));

    let answer = eval(&session, 3, "f(7)");
    assert_eq!(answer.fragments[0].kind, FragmentKind::Statement);
    assert_eq!(answer.output.trim(), "7");

    let answer = eval(&session, 4, "class A { int v = 3; }");
    assert_eq!(answer.fragments[0].kind, FragmentKind::Class);
    assert_eq!(answer.fragments[0].name, "A");

    let answer = eval(&session, 5, "import java.util.List;");
    assert_eq!(answer.fragments[0].kind, FragmentKind::Import);

    let answer = eval(&session, 6, "int y = ;");
    let fragment = &answer.fragments[0];
    assert_eq!(fragment.status, "REJECTED");
    assert_eq!(fragment.diagnostics.len(), 1);
    assert_eq!(fragment.diagnostics[0].line, 1);
    assert_eq!(fragment.diagnostics[0].column, 9);
    assert!(!fragment.diagnostics[0].message.is_empty());

    let answer = eval(&session, 7, "throw new RuntimeException(\"boom\");");
    let exception = answer.fragments[0]
        .exception
        .as_ref()
        .expect("the fragment threw");
    assert_eq!(exception.message, "boom");
    assert!(!exception.stack.is_empty());

    let answer = eval(&session, 8, "System.out.println(\"hello\");");
    assert_eq!(answer.output.trim(), "hello");

    let source = session.completion_source();
    let items: Vec<CompletionItem> = source
        .complete("\"abc\".subs", 10, repl::COMPLETION_TIMEOUT)
        .expect("the helper answers completion");
    assert!(
        items.iter().any(|item| item.text.starts_with("substring")),
        "completion of the console: {items:?}"
    );

    let answer = eval(&session, 9, "new demo.Greeter().hi()");
    assert_eq!(answer.fragments[0].kind, FragmentKind::Expression);
    assert_eq!(answer.fragments[0].value.as_deref(), Some("\"hi\""));

    session
        .send(Command::Eval {
            id: 10,
            text: "while(true){}".to_owned(),
        })
        .expect("the loop starts");
    std::thread::sleep(Duration::from_millis(300));
    session.send(Command::Stop).expect("the stop is written");
    wait(&session, |event| {
        matches!(
            event,
            Event::Done {
                id: 10,
                cancelled: true
            }
        )
    });
    let answer = eval(&session, 11, "1 + 1");
    assert_eq!(answer.fragments[0].value.as_deref(), Some("2"));

    drop(session);
}
