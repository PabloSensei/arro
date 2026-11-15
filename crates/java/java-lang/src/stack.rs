//! The stacks a Java program writes: the frames of a stack trace, the threads of a
//! `jstack` or `jcmd` dump, and `jcmd` itself. The window only draws the report; this
//! module reads it and finds the source of every frame.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use futures_util::StreamExt;
use lang_api::{
    Answer, Confidence, DumpAnswer, Frame, FrameOrigin, LineKind, Provider, Reply, Report,
    SourceId, SourceSpan, StackLine, TextPoint, TextSpan, ThreadStack, TraceQuery,
};
use lsp_client::lsp_types::{OneOf, Uri, WorkspaceSymbolResponse};
use platform::CanonPath;
use project_model::ProjectModel;
use semantic_router::Router;
use tokio::runtime::Handle;

const SOURCE: SourceId = SourceId::new("java-stack");

/// `jcmd` answers in well under this; a hung program must not keep a request alive.
const JCMD_TIMEOUT: Duration = Duration::from_secs(20);
/// Classes one report looks up in the server. A large dump names hundreds of frames,
/// and each lookup is a request to the server.
const MAX_LOOKUPS: usize = 64;
/// Lookups in flight at once: the second answer takes a few rounds instead of one request
/// after another.
const LOOKUP_CONCURRENCY: usize = 8;

/// Reads a pasted stack trace or thread dump. Frames carry no place until the project
/// resolves them.
pub fn parse(text: &str) -> Report {
    if looks_like_dump(text) {
        let (threads, deadlock) = parse_dump(text);
        Report::Dump { threads, deadlock }
    } else {
        Report::Trace {
            lines: text.lines().map(line_of).collect(),
        }
    }
}

/// Whether the text is a thread dump rather than one stack trace.
fn looks_like_dump(text: &str) -> bool {
    let mut header = false;
    let mut state = false;
    let mut bracketed = false;
    let mut frames = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Full thread dump") {
            return true;
        }
        header |= thread_header(trimmed).is_some();
        bracketed |= trimmed.starts_with("Thread[");
        state |= trimmed.starts_with("java.lang.Thread.State:");
        frames |= trimmed.starts_with("at ");
    }
    header && (state || (bracketed && frames))
}

/// One line of a stack trace, or of a thread.
fn line_of(text: &str) -> StackLine {
    if let Some(frame) = parse_frame(text) {
        return StackLine {
            text: text.to_owned(),
            kind: LineKind::Frame,
            frame: Some(frame),
        };
    }
    let kind = if is_structure(text.trim_start()) {
        LineKind::Structure
    } else {
        LineKind::Text
    };
    StackLine {
        text: text.to_owned(),
        kind,
        frame: None,
    }
}

/// A line of the form `at pkg.Cls.method(File.java:42)`.
fn parse_frame(line: &str) -> Option<Frame> {
    let rest = line.trim_start().strip_prefix("at ")?;
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    if close <= open {
        return None;
    }
    let target = rest[..open].trim_end();
    let source = &rest[open + 1..close];
    let (owner, method) = target.rsplit_once('.')?;
    // `java.base@21/java.lang.Thread.run`: the module belongs to neither the class nor
    // the method.
    let owner = strip_module(owner);
    if owner.is_empty() || method.is_empty() {
        return None;
    }
    let (file, line) = source_parts(source);
    Some(Frame {
        class: owner.to_owned(),
        method: method.to_owned(),
        file,
        line,
        place: None,
        origin: FrameOrigin::Unknown,
    })
}

/// The file and the line a frame names, counting from zero. `Native Method`,
/// `Unknown Source` and a file without a line number are frames of a class, just not of
/// a line.
fn source_parts(source: &str) -> (Option<String>, Option<usize>) {
    let source = strip_module(source.trim());
    let (file, number) = match source.rsplit_once(':') {
        Some((file, number)) => (file, number.trim().parse::<u32>().ok()),
        None => (source, None),
    };
    if file.is_empty() || file == "Native Method" || file == "Unknown Source" || file == "null" {
        return (None, None);
    }
    (
        Some(file.to_owned()),
        number
            .filter(|number| *number > 0)
            .map(|number| number as usize - 1),
    )
}

/// A source or a class name without the module prefix, `java.base@21/` for one.
fn strip_module(text: &str) -> &str {
    text.rsplit_once('/').map_or(text, |(_, name)| name)
}

/// `Caused by:`, `Suppressed:` and `... N more` carry the shape of the trace.
fn is_structure(trimmed: &str) -> bool {
    trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || more_count(trimmed).is_some()
}

fn more_count(trimmed: &str) -> Option<u32> {
    let rest = trimmed.strip_prefix("... ")?;
    let mut words = rest.split_whitespace();
    let count: u32 = words.next()?.parse().ok()?;
    (words.next() == Some("more") && words.next().is_none()).then_some(count)
}

/// The threads of a dump and whether the dump reported a deadlock between them.
fn parse_dump(text: &str) -> (Vec<ThreadStack>, bool) {
    let mut threads = Vec::new();
    let mut current: Option<ThreadStack> = None;
    let mut deadlock = false;
    let mut in_deadlock = false;
    let mut deadlocked = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.contains("Found one Java-level deadlock") {
            deadlock = true;
            in_deadlock = true;
            if let Some(thread) = current.take() {
                threads.push(thread);
            }
            continue;
        }
        if in_deadlock {
            // The section repeats the stacks of the threads of the deadlock and names
            // them in the lines around the stacks.
            deadlocked.extend(quoted_names(trimmed));
            continue;
        }
        if let Some(name) = thread_header(trimmed) {
            if let Some(thread) = current.take() {
                threads.push(thread);
            }
            current = Some(ThreadStack {
                name,
                state: String::new(),
                lines: Vec::new(),
                deadlocked: false,
                waiting_for_lock: false,
            });
            continue;
        }
        let Some(thread) = current.as_mut() else {
            continue;
        };
        if let Some(state) = thread_state(trimmed) {
            thread.state = state;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("at ") {
            thread.lines.push(line_of(trimmed));
            continue;
        }
        if let Some(waiting) = lock_note(trimmed) {
            thread.waiting_for_lock |= waiting;
            thread.lines.push(StackLine {
                text: trimmed.to_owned(),
                kind: LineKind::Lock,
                frame: None,
            });
        }
    }
    if let Some(thread) = current.take() {
        threads.push(thread);
    }
    for thread in &mut threads {
        thread.deadlocked = deadlocked.contains(&thread.name);
    }
    (threads, deadlock)
}

/// The name of a thread at the head of its block, `"main" #1 prio=5 …` or
/// `Thread[main,5,main]`.
fn thread_header(trimmed: &str) -> Option<String> {
    if let Some((name, _)) = quoted(trimmed)
        && trimmed.starts_with('"')
    {
        return Some(name);
    }
    let rest = trimmed.strip_prefix("Thread[")?;
    let end = rest.find(']')?;
    let mut parts = rest[..end].rsplitn(3, ',');
    let _priority = parts.next()?;
    let _group = parts.next()?;
    let name = parts.next()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The text and the offset just past the closing quote of the first quoted name.
fn quoted(text: &str) -> Option<(String, usize)> {
    let start = text.find('"')?;
    let mut name = String::new();
    let mut escaped = false;
    for (offset, character) in text[start + 1..].char_indices() {
        if escaped {
            name.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => return Some((name, start + 1 + offset + 1)),
            _ => name.push(character),
        }
    }
    None
}

/// Every quoted name of a line of the deadlock section.
fn quoted_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some((name, at)) = quoted(rest) {
        names.push(name);
        rest = &rest[at..];
    }
    names
}

/// `java.lang.Thread.State: WAITING (on object monitor)` names the state of the thread.
fn thread_state(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("java.lang.Thread.State:")?.trim();
    let state = rest.split([' ', '\t']).next().unwrap_or(rest);
    (!state.is_empty()).then(|| state.to_owned())
}

/// A note under a frame about a monitor, and whether the thread waits for it.
fn lock_note(trimmed: &str) -> Option<bool> {
    if trimmed.starts_with("- waiting to lock") || trimmed.starts_with("- parking to wait for") {
        Some(true)
    } else if trimmed.starts_with("- locked") || trimmed.starts_with("- waiting on") {
        Some(false)
    } else {
        None
    }
}

/// Gives every frame of a class of the project the file of that class.
fn resolve_project(report: &mut Report, roots: &[PathBuf]) {
    let mut classes: HashMap<String, Option<PathBuf>> = HashMap::new();
    for_each_frame(report, |frame| {
        if !classes.contains_key(&frame.class) {
            classes.insert(frame.class.clone(), class_file(&frame.class, roots));
        }
    });
    for_each_frame_mut(report, |frame| {
        let Some(Some(path)) = classes.get(&frame.class) else {
            return;
        };
        frame.origin = FrameOrigin::Project;
        frame.place = frame.line.map(|line| place_of(path, line));
    });
}

/// The source file of `pkg.Cls`, `pkg.Outer` for the nested `pkg.Outer$Inner`.
fn class_file(class: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let (package, simple) = match class.rsplit_once('.') {
        Some((package, simple)) => (package, simple),
        None => ("", class),
    };
    let outer = simple.split('$').next().unwrap_or(simple);
    let mut directory = PathBuf::new();
    for part in package.split('.').filter(|part| !part.is_empty()) {
        directory.push(part);
    }
    let name = format!("{outer}.java");
    roots
        .iter()
        .map(|root| root.join(&directory).join(&name))
        .find(|candidate| candidate.is_file())
}

fn place_of(path: &Path, line: usize) -> SourceSpan {
    let uri = CanonPath::new(path).ok().map_or_else(
        || path.to_string_lossy().into_owned(),
        |canonical| canonical.to_uri(),
    );
    let point = TextPoint {
        line: line as u32,
        character: 0,
    };
    SourceSpan {
        uri,
        span: TextSpan {
            start: point,
            end: point,
        },
    }
}

fn for_each_frame(report: &Report, mut run: impl FnMut(&Frame)) {
    match report {
        Report::Trace { lines } => {
            for frame in lines.iter().filter_map(|line| line.frame.as_ref()) {
                run(frame);
            }
        }
        Report::Dump { threads, .. } => {
            for frame in threads
                .iter()
                .flat_map(|thread| thread.lines.iter())
                .filter_map(|line| line.frame.as_ref())
            {
                run(frame);
            }
        }
    }
}

fn for_each_frame_mut(report: &mut Report, mut run: impl FnMut(&mut Frame)) {
    match report {
        Report::Trace { lines } => {
            for frame in lines.iter_mut().filter_map(|line| line.frame.as_mut()) {
                run(frame);
            }
        }
        Report::Dump { threads, .. } => {
            for frame in threads
                .iter_mut()
                .flat_map(|thread| thread.lines.iter_mut())
                .filter_map(|line| line.frame.as_mut())
            {
                run(frame);
            }
        }
    }
}

/// The classes of the report the project sources did not answer.
fn unresolved(report: &Report) -> Vec<String> {
    let mut classes = BTreeSet::new();
    for_each_frame(report, |frame| {
        if frame.place.is_none() {
            classes.insert(frame.class.clone());
        }
    });
    classes.into_iter().take(MAX_LOOKUPS).collect()
}

/// The place the server knows for each class: a `jdt:` URI for a class of a library or
/// of the runtime, a file for a class of the project. Runs the lookups side by side and
/// keeps the answer of every class the server knew.
async fn lookups(router: &Router, classes: &[String]) -> HashMap<String, SourceSpan> {
    let found = futures_util::stream::iter(classes.iter().cloned())
        .map(|class| async move {
            let place = lookup_class(router, &class).await;
            (class, place)
        })
        .buffer_unordered(LOOKUP_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    found
        .into_iter()
        .filter_map(|(class, place)| place.map(|place| (class, place)))
        .collect()
}

/// The place of one class, `None` when the server does not answer with its symbol.
async fn lookup_class(router: &Router, class: &str) -> Option<SourceSpan> {
    let (package, simple) = split_class(class);
    let outer = simple.split('$').next().unwrap_or(simple);
    let Ok(Some(response)) = router.workspace_symbol(outer).await else {
        return None;
    };
    for (name, container, location) in symbol_places(response) {
        if name != simple && name != outer {
            continue;
        }
        if !container_matches(container.as_deref(), package, outer) {
            continue;
        }
        return Some(location);
    }
    None
}

fn split_class(class: &str) -> (&str, &str) {
    match class.rsplit_once('.') {
        Some((package, simple)) => (package, simple),
        None => ("", class),
    }
}

/// A package matches its class, and so does the package of the enclosing class of a
/// nested one.
fn container_matches(container: Option<&str>, package: &str, outer: &str) -> bool {
    let Some(container) = container else {
        return package.is_empty();
    };
    if package.is_empty() {
        return true;
    }
    container == package
        || container.ends_with(&format!(".{package}"))
        || container == format!("{package}.{outer}")
        || container.ends_with(&format!(".{package}.{outer}"))
}

fn symbol_places(response: WorkspaceSymbolResponse) -> Vec<(String, Option<String>, SourceSpan)> {
    match response {
        WorkspaceSymbolResponse::Flat(symbols) => symbols
            .into_iter()
            .filter_map(|symbol| {
                let span = location_span(&symbol.location)?;
                Some((symbol.name, symbol.container_name, span))
            })
            .collect(),
        WorkspaceSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .filter_map(|symbol| {
                let OneOf::Left(location) = symbol.location else {
                    return None;
                };
                let span = location_span(&location)?;
                Some((symbol.name, symbol.container_name, span))
            })
            .collect(),
    }
}

fn location_span(location: &lsp_client::lsp_types::Location) -> Option<SourceSpan> {
    Some(SourceSpan {
        uri: location.uri.as_str().to_owned(),
        span: TextSpan {
            start: TextPoint {
                line: location.range.start.line,
                character: location.range.start.character,
            },
            end: TextPoint {
                line: location.range.end.line,
                character: location.range.end.character,
            },
        },
    })
}

/// Fills the frames the server knows, with the line the frame named. Answers whether a
/// frame got a place.
fn apply_lookups(report: &mut Report, found: &HashMap<String, SourceSpan>, root: &Path) -> bool {
    let origin = |uri: &str| {
        if file_under(uri, root) {
            FrameOrigin::Project
        } else {
            FrameOrigin::Library
        }
    };
    let mut applied = false;
    for_each_frame_mut(report, |frame| {
        let Some(span) = found.get(&frame.class) else {
            return;
        };
        frame.origin = origin(&span.uri);
        frame.place = frame.line.map(|line| SourceSpan {
            uri: span.uri.clone(),
            span: TextSpan {
                start: TextPoint {
                    line: line as u32,
                    character: 0,
                },
                end: TextPoint {
                    line: line as u32,
                    character: 0,
                },
            },
        });
        applied = true;
    });
    applied
}

fn file_under(uri: &str, root: &Path) -> bool {
    let Ok(uri) = Uri::from_str(uri) else {
        return false;
    };
    lsp_client::uri::file_path(&uri).is_some_and(|path| path.starts_with(root))
}

/// The text of `jcmd <pid> Thread.print` from the JDK at `home`. Blocks, so it runs off
/// the UI thread.
pub fn jcmd_thread_dump(home: &Path, pid: u32) -> Result<String, String> {
    let jcmd = home.join("bin").join(base::program::executable("jcmd"));
    if !jcmd.is_file() {
        return Err(format!(
            "The JDK of the project has no jcmd ({}). A plain runtime cannot dump the threads of a program: choose a JDK in Project Structure.",
            jcmd.display()
        ));
    }
    let mut child = Command::new(&jcmd)
        .arg(pid.to_string())
        .arg("Thread.print")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run {}: {error}", jcmd.display()))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(stream) = stdout.as_mut() {
            let _ = stream.read_to_end(&mut text);
        }
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(stream) = stderr.as_mut() {
            let _ = stream.read_to_end(&mut text);
        }
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < JCMD_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "jcmd did not answer in {} s",
                    JCMD_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(format!("jcmd failed: {error}")),
        }
    };
    let text = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    let errors = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    if !status.success() {
        let reason = if errors.trim().is_empty() {
            text
        } else {
            errors
        };
        return Err(format!("jcmd failed: {}", reason.trim()));
    }
    if text.trim().is_empty() {
        return Err("jcmd answered with no thread dump".to_owned());
    }
    Ok(text)
}

/// The JDK of the project, or the reason its threads cannot be dumped.
fn dump_text(root: &Path, pid: u32) -> Result<String, String> {
    let environment = java_project::resolve_environment(root);
    let Some(jdk) = environment.selected else {
        return Err(
            "The project has no JDK: choose one in Project Structure to dump the threads of its programs."
                .to_owned(),
        );
    };
    jcmd_thread_dump(&jdk.home, pid)
}

/// Reads stack traces and thread dumps of one open project, and asks a running program
/// of the project for the stacks of its threads.
#[derive(Clone)]
pub(crate) struct StackService {
    root: PathBuf,
    router: Arc<Router>,
    runtime: Handle,
    model: Arc<Mutex<Option<Arc<ProjectModel>>>>,
}

impl StackService {
    pub(crate) fn new(
        root: PathBuf,
        router: Arc<Router>,
        runtime: Handle,
        model: Arc<Mutex<Option<Arc<ProjectModel>>>>,
    ) -> Self {
        Self {
            root,
            router,
            runtime,
            model,
        }
    }

    fn source_roots(&self) -> Vec<PathBuf> {
        let model = self
            .model
            .lock()
            .map(|model| model.clone())
            .unwrap_or_default();
        let Some(model) = model else {
            return Vec::new();
        };
        let mut roots: Vec<PathBuf> = model
            .modules
            .iter()
            .flat_map(|module| module.units.iter())
            .flat_map(|unit| unit.source_roots.iter().cloned())
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    /// Reads the text and finds the files of the project's classes. The files of the
    /// project are read off the runtime thread, so the first answer comes at once.
    async fn project_report(&self, text: &str) -> Report {
        let parsed = parse(text);
        let roots = self.source_roots();
        let resolved = tokio::task::spawn_blocking(move || {
            let mut report = parsed;
            resolve_project(&mut report, &roots);
            report
        })
        .await;
        resolved.unwrap_or_else(|_| parse(text))
    }

    /// Fills the frames of the libraries and of the runtime. Answers whether a frame got
    /// a place: the report is worth a second answer only then.
    async fn refine(&self, report: &mut Report) -> bool {
        let classes = unresolved(report);
        if classes.is_empty() {
            return false;
        }
        let found = lookups(&self.router, &classes).await;
        apply_lookups(report, &found, &self.root)
    }
}

impl Provider<TraceQuery, Report> for StackService {
    fn start(&self, query: TraceQuery, _: Cancel, mut reply: Reply<Report>) {
        let service = self.clone();
        self.runtime.spawn(async move {
            // The text and the frames of the project are ready first: the window shows
            // them while the server looks up the classes of the libraries.
            let mut report = service.project_report(&query.0).await;
            let first = report.clone();
            if reply
                .send(Answer::new(first, Confidence::Partial, SOURCE))
                .is_err()
            {
                return;
            }
            if service.refine(&mut report).await {
                let _ = reply.send(Answer::new(report, Confidence::Certain, SOURCE));
            }
        });
    }
}

impl Provider<lang_api::DumpQuery, DumpAnswer> for StackService {
    fn start(&self, query: lang_api::DumpQuery, _: Cancel, mut reply: Reply<DumpAnswer>) {
        let service = self.clone();
        self.runtime.spawn(async move {
            let root = service.root.clone();
            let pid = query.0;
            let text = tokio::task::spawn_blocking(move || dump_text(&root, pid)).await;
            let text = match text {
                Ok(Ok(text)) => text,
                Ok(Err(error)) => {
                    let answer = DumpAnswer {
                        report: None,
                        error: Some(error),
                    };
                    let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
                    return;
                }
                Err(error) => {
                    let answer = DumpAnswer {
                        report: None,
                        error: Some(format!("The thread dump did not finish: {error}")),
                    };
                    let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
                    return;
                }
            };
            // The threads and their project frames first, the library places after them.
            let mut report = service.project_report(&text).await;
            let first = DumpAnswer {
                report: Some(report.clone()),
                error: None,
            };
            if reply
                .send(Answer::new(first, Confidence::Partial, SOURCE))
                .is_err()
            {
                return;
            }
            if service.refine(&mut report).await {
                let answer = DumpAnswer {
                    report: Some(report),
                    error: None,
                };
                let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_names_its_class_method_file_and_line() {
        let frame = parse_frame("\tat com.example.Main.method(Main.java:42)").unwrap();
        assert_eq!(frame.class, "com.example.Main");
        assert_eq!(frame.method, "method");
        assert_eq!(frame.line, Some(41));
    }

    #[test]
    fn native_and_unknown_sources_are_frames_without_a_line() {
        let native = parse_frame("\tat java.lang.Object.wait0(Native Method)").unwrap();
        assert_eq!(native.class, "java.lang.Object");
        assert_eq!(native.method, "wait0");
        assert_eq!(native.line, None);

        let unknown = parse_frame("\tat com.example.Main.lost(Unknown Source)").unwrap();
        assert_eq!(unknown.class, "com.example.Main");
        assert_eq!(unknown.line, None);
    }

    #[test]
    fn a_module_prefix_is_not_a_part_of_the_class_or_of_the_source() {
        let frame =
            parse_frame("\tat java.base@21/java.lang.Thread.run(Thread.java:1583)").unwrap();
        assert_eq!(frame.class, "java.lang.Thread");
        assert_eq!(frame.method, "run");
        assert_eq!(frame.line, Some(1582));

        let source =
            parse_frame("\tat java.lang.Thread.run(java.base@21/Thread.java:1583)").unwrap();
        assert_eq!(source.class, "java.lang.Thread");
        assert_eq!(source.line, Some(1582));

        let native =
            parse_frame("\tat jdk.internal.misc.Unsafe.park(java.base@21/Native Method)").unwrap();
        assert_eq!(native.class, "jdk.internal.misc.Unsafe");
        assert_eq!(native.line, None);
    }

    #[test]
    fn lambdas_and_nested_classes_keep_their_names() {
        let lambda = parse_frame("\tat com.example.Main.lambda$main$0(Main.java:12)").unwrap();
        assert_eq!(lambda.class, "com.example.Main");
        assert_eq!(lambda.method, "lambda$main$0");

        let nested = parse_frame("\tat com.example.Outer$Inner.run(Outer.java:7)").unwrap();
        assert_eq!(nested.class, "com.example.Outer$Inner");
        assert_eq!(nested.method, "run");
    }

    #[test]
    fn caused_by_suppressed_and_more_are_structure_and_not_frames() {
        let report = parse(
            "Caused by: java.io.IOException: bad\n\tSuppressed: java.lang.Error\n\t... 3 more",
        );
        let Report::Trace { lines } = report else {
            panic!("a trace");
        };
        let kinds: Vec<_> = lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Structure,
                LineKind::Structure,
                LineKind::Structure
            ]
        );
        assert!(lines.iter().all(|line| line.frame.is_none()));
    }

    #[test]
    fn a_dump_of_jdk_17_has_its_threads_states_and_lock_notes() {
        let (threads, deadlock) = parse_dump(include_str!("../tests/data/threads-jdk17.txt"));

        assert!(!deadlock);
        let main = threads.iter().find(|thread| thread.name == "main").unwrap();
        assert_eq!(main.state, "TIMED_WAITING");
        assert!(main.has_frames());
        let worker = threads
            .iter()
            .find(|thread| thread.name == "pool-1-thread-1")
            .unwrap();
        assert_eq!(worker.state, "WAITING");
        assert!(worker.waiting_for_lock);
        assert!(
            worker
                .lines
                .iter()
                .any(|line| line.kind == LineKind::Lock
                    && line.text.starts_with("- waiting to lock")),
            "the lock note is kept: {:?}",
            worker.lines
        );
        assert!(
            worker
                .lines
                .iter()
                .any(|line| line.kind == LineKind::Lock && line.text.starts_with("- locked")),
            "the held lock is kept: {:?}",
            worker.lines
        );
    }

    #[test]
    fn a_dump_of_jdk_21_reads_the_module_inside_the_frame() {
        let (threads, deadlock) = parse_dump(include_str!("../tests/data/threads-jdk21.txt"));

        assert!(!deadlock);
        let main = threads.iter().find(|thread| thread.name == "main").unwrap();
        assert_eq!(main.state, "WAITING");
        let frame = main
            .lines
            .iter()
            .find_map(|line| line.frame.as_ref())
            .unwrap();
        assert_eq!(frame.class, "jdk.internal.misc.Unsafe");
        assert_eq!(frame.line, None);
    }

    #[test]
    fn the_format_of_get_all_stack_traces_is_a_dump() {
        let text = "Thread[main,5,main]\n\tat example.Main.main(Main.java:3)\n\nThread[worker,5,main]\n\tat example.Main.work(Main.java:9)\n";
        let report = parse(text);

        let Report::Dump { threads, .. } = report else {
            panic!("a dump");
        };
        let names: Vec<&str> = threads.iter().map(|thread| thread.name.as_str()).collect();
        assert_eq!(names, ["main", "worker"]);
        assert_eq!(
            threads[1].lines[0].frame.as_ref().unwrap().class,
            "example.Main"
        );
    }

    #[test]
    fn a_deadlock_section_marks_the_threads_it_names() {
        let (threads, deadlock) = parse_dump(include_str!("../tests/data/threads-deadlock.txt"));

        assert!(deadlock);
        let marked: Vec<&str> = threads
            .iter()
            .filter(|thread| thread.deadlocked)
            .map(|thread| thread.name.as_str())
            .collect();
        assert_eq!(marked, ["Thread-0", "Thread-1"]);
    }

    #[test]
    fn frames_of_the_project_find_their_file() {
        let root = test_support::scratch("lang-stack-root");
        let source = root.join("src/main/java/com/example");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Main.java"), "class Main {}\n").unwrap();
        let mut report = parse("\tat com.example.Main.main(Main.java:5)");
        resolve_project(&mut report, &[root.join("src/main/java")]);

        let Report::Trace { lines } = report else {
            panic!("a trace");
        };
        let frame = lines[0].frame.as_ref().unwrap();
        assert_eq!(frame.origin, FrameOrigin::Project);
        let place = frame.place.as_ref().unwrap();
        assert!(
            place.uri.ends_with("/com/example/Main.java"),
            "{}",
            place.uri
        );
        assert_eq!(place.span.start.line, 4);
    }

    #[test]
    fn a_class_outside_the_project_sources_has_no_place() {
        let root = test_support::scratch("lang-stack-missing");
        let mut report = parse("\tat com.example.Nowhere.main(Nowhere.java:5)");
        resolve_project(&mut report, &[root]);

        let Report::Trace { lines } = report else {
            panic!("a trace");
        };
        let frame = lines[0].frame.as_ref().unwrap();
        assert_eq!(frame.origin, FrameOrigin::Unknown);
        assert!(frame.place.is_none());
    }

    #[test]
    fn a_home_without_jcmd_says_so() {
        let home = test_support::scratch("lang-stack-jre");
        std::fs::create_dir_all(home.join("bin")).unwrap();

        let error = jcmd_thread_dump(&home, 42).unwrap_err();

        assert!(error.contains("jcmd"), "{error}");
        assert!(error.contains("JDK"), "{error}");
    }

    /// Runs a program with two threads on the JDK of the corpus and dumps them with
    /// `jcmd`, as the Run window does.
    #[test]
    #[ignore = "needs IDE_CORPUS with jdk/21"]
    fn a_real_jdk_21_dumps_two_threads() {
        let corpus = std::env::var("IDE_CORPUS").expect("set IDE_CORPUS");
        let home = PathBuf::from(corpus).join("jdk").join("21");
        let home = std::fs::canonicalize(home).expect("the JDK of the corpus");
        let work = test_support::TempDir::new("lang-stack-real");
        let file = work.path().join("Two.java");
        std::fs::write(
            &file,
            r#"public class Two {
    static final Object LOCK = new Object();

    public static void main(String[] args) throws Exception {
        Thread waiter = new Thread(() -> {
            synchronized (LOCK) {
                try {
                    LOCK.wait();
                } catch (InterruptedException ignored) {
                }
            }
        }, "waiter");
        waiter.start();
        Thread.sleep(300);
        System.out.println("READY");
        System.out.flush();
        Thread.sleep(60000);
    }
}
"#,
        )
        .unwrap();
        let compiled = Command::new(home.join("bin").join(base::program::executable("javac")))
            .arg("-d")
            .arg(work.path())
            .arg(&file)
            .status()
            .expect("javac runs");
        assert!(compiled.success(), "the program compiles");
        let mut child = Command::new(home.join("bin").join(base::program::executable("java")))
            .arg("-cp")
            .arg(work.path())
            .arg("Two")
            .stdout(Stdio::piped())
            .spawn()
            .expect("java runs");
        let mut stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        let mut reader = std::io::BufReader::new(&mut stdout);
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        assert!(line.contains("READY"), "{line}");

        let text = jcmd_thread_dump(&home, child.id()).expect("jcmd answers");
        let report = parse(&text);
        let _ = child.kill();
        let _ = child.wait();

        let Report::Dump { threads, .. } = report else {
            panic!("a thread dump");
        };
        let waiter = threads
            .iter()
            .find(|thread| thread.name == "waiter")
            .expect("the waiting thread is in the dump");
        assert_eq!(waiter.state, "WAITING", "{:?}", waiter.lines);
        assert!(
            threads.iter().any(|thread| thread.name == "main"),
            "the main thread is in the dump"
        );
    }
}
