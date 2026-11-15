//! Profile runs on Flight Recorder: the recording flag of a run, the helper that turns a
//! recording into the samples, trees and timeline of the window, and the recordings the
//! window lists.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base::Result;
use base::program::executable;
use lang_api::{
    ProfileFrame, ProfileNode, ProfileRecording, ProfileReport, ProfileSample, ProfileSamples,
    ProfileSnapshot, ProfileType, ProfileTypeTree, ProfileUnit,
};
use project_model::ProjectModel;
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::{BuildEnv, Jdk};

/// The folder under the cache root that holds the recordings of the profiler.
pub const PROFILE_FOLDER: &str = "profiler";
/// The recordings the window lists and the folder keeps.
pub const MAX_RECORDINGS: usize = 10;
/// The samples a snapshot reads. A long recording of a busy program has millions; the
/// window shows the first of them and says so.
pub const MAX_SAMPLES: u64 = 50_000;
/// The first update of the JDK 8 line that carries Flight Recorder.
const JFR_UPDATE: u32 = 262;
/// The events of the recording the profiler reads. A JDK that does not know one of them
/// prints the others.
const EVENTS: &str = "jdk.ExecutionSample,jdk.ObjectAllocationSample,jdk.ObjectAllocationInNewTLAB,jdk.ObjectAllocationOutsideTLAB";

/// A fresh recording file for a run. The folder keeps the newest recordings only, so the
/// state folder does not grow without end.
pub fn recording_file(cache_root: &Path, name: &str) -> Result<PathBuf> {
    let directory = cache_root.join(PROFILE_FOLDER);
    fs::create_dir_all(&directory)?;
    let mut older: Vec<(SystemTime, PathBuf)> = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "jfr"))
                .map(|path| {
                    let modified = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(UNIX_EPOCH);
                    (modified, path)
                })
                .collect()
        })
        .unwrap_or_default();
    older.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in older.into_iter().skip(MAX_RECORDINGS - 1) {
        fs::remove_file(path).ok();
    }
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    Ok(directory.join(format!("{stem}-{millis}.jfr")))
}

/// The recordings of the folder, the newest first, as many as the window lists.
pub fn recordings(cache_root: &Path) -> Vec<ProfileRecording> {
    let directory = cache_root.join(PROFILE_FOLDER);
    let mut found: Vec<(SystemTime, ProfileRecording)> = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "jfr"))
                .map(|path| {
                    let modified = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(UNIX_EPOCH);
                    (modified, ProfileRecording::named(path))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.label.cmp(&right.1.label))
    });
    found
        .into_iter()
        .take(MAX_RECORDINGS)
        .map(|(_, recording)| recording)
        .collect()
}

/// Every file the run of `recording` left in its folder. Flight Recorder does not put the
/// process id into the file name, so a run that starts several JVMs writes one file; a
/// tool that gives each JVM its own name leaves several of them with the same prefix.
pub fn run_recordings(recording: &Path) -> Vec<PathBuf> {
    let Some(directory) = recording.parent() else {
        return vec![recording.to_owned()];
    };
    let Some(stem) = recording.file_stem().and_then(|stem| stem.to_str()) else {
        return vec![recording.to_owned()];
    };
    let prefix = stem;
    let mut found: Vec<PathBuf> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "jfr"))
                .filter(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem.starts_with(prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    if found.is_empty() {
        found.push(recording.to_owned());
    }
    found.sort();
    found
}

/// Whether the JVM of `jdk` records samples by itself, which the JDK 8 line got in 8u262.
pub fn supported(jdk: &Jdk) -> bool {
    if jdk.feature > 8 {
        return true;
    }
    jdk.feature == 8 && update_of(&jdk.version).is_some_and(|update| update >= JFR_UPDATE)
}

fn update_of(version: &str) -> Option<u32> {
    version.rsplit('_').next()?.parse().ok()
}

fn jfr_tool(jdk: &Jdk) -> PathBuf {
    jdk.home.join("bin").join(executable("jfr"))
}

/// The argument that starts a recording of the run and writes it when the JVM ends. The
/// path is quoted, so a folder with commas and spaces works too.
pub fn recording_flag(recording: &Path) -> String {
    format!(
        "-XX:StartFlightRecording=filename=\"{}\",settings=profile,dumponexit=true",
        recording.display()
    )
}

/// Reads `recording` with the helper of the JDK into the report of the window, in a single
/// pass over the output: a long recording is larger than memory, and the helper writes it
/// as one document.
pub fn report(
    env: &BuildEnv,
    jdk: &Jdk,
    recording: &Path,
    model: &ProjectModel,
) -> Result<ProfileReport> {
    parse(env, jdk, recording, model)
}

/// Reads a recording with the JDK of the project.
pub fn report_of(env: &BuildEnv, recording: &Path, model: &ProjectModel) -> Result<ProfileReport> {
    let jdk = env.project_jdk(&model.root)?;
    report(env, &jdk, recording, model)
}

/// The CPU tree alone, for a provider that has nothing else to show.
pub fn snapshot(
    env: &BuildEnv,
    jdk: &Jdk,
    recording: &Path,
    model: &ProjectModel,
) -> Result<ProfileSnapshot> {
    parse(env, jdk, recording, model).map(|report| report.cpu_tree)
}

fn parse(
    env: &BuildEnv,
    jdk: &Jdk,
    recording: &Path,
    model: &ProjectModel,
) -> Result<ProfileReport> {
    if !supported(jdk) {
        return Err(format!(
            "the profiler needs Flight Recorder, which JDK {} does not have; use 8u{JFR_UPDATE} or newer",
            jdk.version
        )
        .into());
    }
    if !recording.is_file() {
        return Err(format!("no recording at {}", recording.display()).into());
    }
    let tool = jfr_tool(jdk);
    if !tool.is_file() {
        return Err(format!("no recording tool at {}", tool.display()).into());
    }
    let spec = proc_sup::ProcessSpec::new(&tool)
        .cwd(model.root.clone())
        .args(["print", "--json", "--events", EVENTS])
        .arg(recording);
    let mut process = env
        .apply(spec, jdk)
        .start()
        .map_err(|error| format!("no profile: {error}"))?;
    drop(process.take_stdin());
    let stderr = process
        .take_stderr()
        .map(|stderr| std::thread::spawn(move || read_text(stderr)));
    let Some(stdout) = process.take_stdout() else {
        return Err("no profile: the helper writes nothing".into());
    };
    let parsed = parse_report(BufReader::new(stdout), model, MAX_SAMPLES);
    let status = process
        .wait()
        .map_err(|error| format!("no profile: {error}"))?;
    let stderr = stderr
        .map(|reader| reader.join().unwrap_or_default())
        .unwrap_or_default();
    match parsed {
        // A run that stopped at the limit closes the pipe, so the helper may end with an
        // error after it wrote what the window shows.
        Ok(report)
            if report.cpu_tree.truncated || snapshot_truncated(&report) || status.success() =>
        {
            Ok(report)
        }
        Ok(_) => Err(format!("no profile: exit {:?}\n{}", status.code(), tail(&stderr)).into()),
        Err(error) => Err(format!("no profile: {error}\n{}", tail(&stderr)).into()),
    }
}

/// Whether a limit stopped the reading before the helper ended, which closes its pipe.
fn snapshot_truncated(report: &ProfileReport) -> bool {
    report
        .allocations
        .as_ref()
        .is_some_and(|tree| tree.truncated)
}

fn read_text(mut reader: impl Read) -> String {
    let mut text = String::new();
    reader.read_to_string(&mut text).ok();
    text
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// The samples of the helper output folded into the trees, the types and the timeline.
/// The output is read as it comes, so the whole document never sits in a string.
pub fn parse_report(reader: impl Read, model: &ProjectModel, limit: u64) -> Result<ProfileReport> {
    let mut builder = Builder::new(model, limit);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    RootSeed {
        builder: &mut builder,
    }
    .deserialize(&mut deserializer)
    .map_err(|error| format!("bad recording: {error}"))?;
    Ok(builder.finish())
}

/// The CPU tree alone, for a test or a provider that reads no allocations.
pub fn parse_reader(
    reader: impl Read,
    model: &ProjectModel,
    limit: u64,
) -> Result<ProfileSnapshot> {
    parse_report(reader, model, limit).map(|report| report.cpu_tree)
}

/// The stacks of the helper output folded into a tree. The output is read as it comes,
/// so the whole document never sits in a string.
struct Builder<'a> {
    model: &'a ProjectModel,
    /// The source of a class, `None` for the runtime and libraries. A recording names the
    /// same classes many times, so each is looked up once.
    sources: HashMap<String, Option<PathBuf>>,
    /// The frames the samples of the CPU refer to, by index.
    frames: Vec<ProfileFrame>,
    frame_ids: HashMap<(String, String, String), u32>,
    lanes: Vec<String>,
    lane_ids: HashMap<String, u32>,
    cpu: Vec<ProfileSample>,
    /// The allocation samples folded into one tree by weight.
    allocation_roots: Vec<ProfileNode>,
    allocation_weight: u64,
    /// The tree and the totals of one allocated class.
    allocated: HashMap<String, Allocated>,
    /// The moment of the first sample, in milliseconds since the epoch.
    first_time: Option<i64>,
    last_time: u64,
    total: u64,
    limit: u64,
    truncated: bool,
}

struct Allocated {
    package: String,
    source: Option<PathBuf>,
    weight: u64,
    count: u64,
    roots: Vec<ProfileNode>,
}

impl<'a> Builder<'a> {
    fn new(model: &'a ProjectModel, limit: u64) -> Self {
        Self {
            model,
            sources: HashMap::new(),
            frames: Vec::new(),
            frame_ids: HashMap::new(),
            lanes: Vec::new(),
            lane_ids: HashMap::new(),
            cpu: Vec::new(),
            allocation_roots: Vec::new(),
            allocation_weight: 0,
            allocated: HashMap::new(),
            first_time: None,
            last_time: 0,
            total: 0,
            limit,
            truncated: false,
        }
    }

    fn finish(self) -> ProfileReport {
        let Builder {
            frames,
            lanes,
            cpu,
            allocation_roots,
            allocation_weight,
            allocated,
            truncated,
            ..
        } = self;
        let cpu_samples = ProfileSamples {
            frames: frames.clone(),
            lanes,
            samples: cpu,
            unit: ProfileUnit::Samples,
        };
        let mut cpu_tree = cpu_samples.snapshot();
        cpu_tree.truncated = truncated;
        let allocations = (allocation_weight > 0).then_some(ProfileSnapshot {
            roots: allocation_roots,
            samples: allocation_weight,
            truncated,
        });
        let mut types: Vec<(String, Allocated)> = allocated.into_iter().collect();
        types.sort_by(|left, right| {
            right
                .1
                .weight
                .cmp(&left.1.weight)
                .then_with(|| right.1.count.cmp(&left.1.count))
                .then_with(|| left.0.cmp(&right.0))
        });
        let types = types
            .into_iter()
            .map(|(class, allocated)| ProfileTypeTree {
                allocated: ProfileType {
                    class,
                    package: allocated.package,
                    source: allocated.source,
                    weight: allocated.weight,
                    count: allocated.count,
                },
                tree: ProfileSnapshot {
                    roots: allocated.roots,
                    samples: allocated.weight,
                    truncated,
                },
            })
            .collect();
        ProfileReport {
            cpu_tree,
            cpu: ProfileSamples {
                frames,
                ..cpu_samples
            },
            allocations,
            types,
        }
    }

    fn push(&mut self, kind: Kind, values: &RawValues) {
        let Some(raw) = values.stack.as_ref() else {
            return;
        };
        if raw.frames.is_empty() {
            return;
        }
        let stack: Vec<u32> = raw
            .frames
            .iter()
            .rev()
            .filter_map(|frame| {
                let id = self.frame(frame)?;
                Some(id)
            })
            .collect();
        if stack.is_empty() {
            return;
        }
        let at = self.time(values.start.as_deref());
        self.total += 1;
        match kind {
            Kind::Cpu => {
                let lane = self.lane(values.sampled.as_ref().or(values.event.as_ref()));
                self.cpu.push(ProfileSample {
                    at,
                    lane,
                    weight: 1,
                    stack,
                });
            }
            Kind::Allocation => {
                let Some(weight) = values.weight.or(values.allocation_size) else {
                    return;
                };
                if weight == 0 {
                    return;
                }
                let Some(class) = values.object.as_ref().map(|object| object.name.clone()) else {
                    return;
                };
                let class = class.replace('/', ".");
                let frames: Vec<ProfileFrame> = stack
                    .iter()
                    .filter_map(|id| self.frames.get(*id as usize))
                    .cloned()
                    .collect();
                let frames: Vec<&ProfileFrame> = frames.iter().collect();
                insert(&mut self.allocation_roots, &frames, 0, weight);
                self.allocation_weight += weight;
                if !self.allocated.contains_key(&class) {
                    let package = values
                        .object
                        .as_ref()
                        .and_then(|object| object.package.as_ref())
                        .map(|package| package.name.replace('/', "."))
                        .unwrap_or_default();
                    let source = self.source(&class);
                    self.allocated.insert(
                        class.clone(),
                        Allocated {
                            package,
                            source,
                            weight: 0,
                            count: 0,
                            roots: Vec::new(),
                        },
                    );
                }
                if let Some(entry) = self.allocated.get_mut(&class) {
                    entry.weight += weight;
                    entry.count += 1;
                    insert(&mut entry.roots, &frames, 0, weight);
                }
            }
        }
    }

    /// The index of a frame, added on its first appearance. A frame without a method, as
    /// a native stub has, is not part of a stack.
    fn frame(&mut self, raw: &RawFrame) -> Option<u32> {
        if raw.method.name.is_empty() {
            return None;
        }
        let class = raw.method.kind.name.replace('/', ".");
        let key = (
            class.clone(),
            raw.method.name.clone(),
            raw.method.descriptor.clone(),
        );
        if let Some(id) = self.frame_ids.get(&key) {
            return Some(*id);
        }
        let source = self.source(&class);
        let package = raw
            .method
            .kind
            .package
            .as_ref()
            .map(|package| package.name.replace('/', "."))
            .unwrap_or_default();
        let frame = ProfileFrame {
            package,
            class,
            method: raw.method.name.clone(),
            signature: raw.method.descriptor.clone(),
            source,
            line: raw
                .line_number
                .and_then(|line| u32::try_from(line).ok())
                .filter(|line| *line > 0),
        };
        self.frames.push(frame);
        let id = (self.frames.len() - 1) as u32;
        self.frame_ids.insert(key, id);
        Some(id)
    }

    fn source(&mut self, class: &str) -> Option<PathBuf> {
        match self.sources.get(class) {
            Some(source) => source.clone(),
            None => {
                let source = source_of(self.model, class);
                self.sources.insert(class.to_owned(), source.clone());
                source
            }
        }
    }

    fn lane(&mut self, thread: Option<&RawThread>) -> u32 {
        let name = thread
            .and_then(|thread| {
                thread
                    .name
                    .as_deref()
                    .or(thread.os.as_deref())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or("unknown")
            .to_owned();
        match self.lane_ids.get(&name) {
            Some(id) => *id,
            None => {
                self.lanes.push(name.clone());
                let id = (self.lanes.len() - 1) as u32;
                self.lane_ids.insert(name, id);
                id
            }
        }
    }

    /// Milliseconds from the first sample of the recording.
    fn time(&mut self, start: Option<&str>) -> u64 {
        let Some(millis) = start.and_then(parse_time) else {
            return self.last_time;
        };
        let first = *self.first_time.get_or_insert(millis);
        let at = millis.saturating_sub(first).max(0) as u64;
        self.last_time = self.last_time.max(at);
        at
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Cpu,
    Allocation,
}

fn insert(nodes: &mut Vec<ProfileNode>, stack: &[&ProfileFrame], at: usize, weight: u64) {
    let frame = stack[at];
    let index = match nodes.iter().position(|node| same_frame(&node.frame, frame)) {
        Some(index) => index,
        None => {
            nodes.push(ProfileNode {
                frame: frame.clone(),
                own: 0,
                total: 0,
                children: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    let node = &mut nodes[index];
    node.total += weight;
    if at + 1 == stack.len() {
        node.own += weight;
        return;
    }
    insert(&mut node.children, stack, at + 1, weight);
}

fn same_frame(one: &ProfileFrame, other: &ProfileFrame) -> bool {
    one.class == other.class && one.method == other.method && one.signature == other.signature
}

/// The source of a class of the project, found under the source roots of the model. A
/// nested class lives in the file of its outermost class.
pub(crate) fn source_of(model: &ProjectModel, class: &str) -> Option<PathBuf> {
    if class.is_empty() || class.starts_with('[') {
        return None;
    }
    let outer = class.split('$').next().unwrap_or(class);
    let relative = format!("{}.java", outer.replace('.', "/"));
    for module in &model.modules {
        for unit in &module.units {
            for root in &unit.source_roots {
                let path = root.join(&relative);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Milliseconds since the epoch of the time the helper writes, such as
/// `2026-11-14T21:25:18.704612700+01:00`.
fn parse_time(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let (clock, zone) = match rest.find(['+', 'Z']) {
        Some(at) => (&rest[..at], Some(&rest[at..])),
        None => match rest.rfind('-') {
            Some(at) => (&rest[..at], Some(&rest[at..])),
            None => (rest, None),
        },
    };
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let (second, millis) = split_seconds(clock.next()?)?;
    let offset = match zone {
        Some("Z") | None => 0,
        Some(zone) => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let mut parts = zone[1..].split(':');
            let hours: i64 = parts.next().unwrap_or("0").parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            sign * (hours * 60 + minutes)
        }
    };
    let days = days_from_civil(year, month, day);
    let total =
        days * 86_400_000 + (hour * 3600 + minute * 60 + second) * 1000 + millis - offset * 60_000;
    Some(total)
}

/// The whole seconds and the milliseconds of a clock field such as `18.704612700`.
fn split_seconds(text: &str) -> Option<(i64, i64)> {
    let (seconds, fraction) = text.split_once('.').unwrap_or((text, ""));
    let seconds: i64 = seconds.parse().ok()?;
    let fraction: String = fraction.chars().take(3).collect();
    let fraction = format!("{fraction:0<3}");
    let millis: i64 = fraction.parse().ok()?;
    Some((seconds, millis))
}

/// Days since 1970-01-01 of a civil date, as the Gregorian calendar counts it.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_of_year = (month + 9) % 12;
    let day_of_year = (153 * month_of_year + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

struct RootSeed<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> DeserializeSeed<'de> for RootSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_map(RootVisitor {
            builder: self.builder,
        })
    }
}

struct RootVisitor<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> Visitor<'de> for RootVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("the document of a recording")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "recording" {
                map.next_value_seed(RecordingSeed {
                    builder: &mut *self.builder,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct RecordingSeed<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> DeserializeSeed<'de> for RecordingSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_map(RecordingVisitor {
            builder: self.builder,
        })
    }
}

struct RecordingVisitor<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> Visitor<'de> for RecordingVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a recording")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "events" {
                map.next_value_seed(EventsSeed {
                    builder: &mut *self.builder,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct EventsSeed<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> DeserializeSeed<'de> for EventsSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_seq(EventsVisitor {
            builder: self.builder,
        })
    }
}

struct EventsVisitor<'a, 'b> {
    builder: &'b mut Builder<'a>,
}

impl<'de> Visitor<'de> for EventsVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("the events of a recording")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(event) = seq.next_element::<RawEvent>()? {
            let kind = match event.kind.as_str() {
                "jdk.ExecutionSample" => Some(Kind::Cpu),
                "jdk.ObjectAllocationSample"
                | "jdk.ObjectAllocationInNewTLAB"
                | "jdk.ObjectAllocationOutsideTLAB" => Some(Kind::Allocation),
                _ => None,
            };
            let Some(kind) = kind else {
                continue;
            };
            if self.builder.total >= self.builder.limit {
                self.builder.truncated = true;
                break;
            }
            if let Some(values) = &event.values {
                self.builder.push(kind, values);
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    values: Option<RawValues>,
}

#[derive(Deserialize)]
struct RawValues {
    #[serde(default, rename = "startTime")]
    start: Option<String>,
    #[serde(default, rename = "stackTrace")]
    stack: Option<RawStack>,
    #[serde(default, rename = "sampledThread")]
    sampled: Option<RawThread>,
    #[serde(default, rename = "eventThread")]
    event: Option<RawThread>,
    #[serde(default)]
    weight: Option<u64>,
    #[serde(default, rename = "allocationSize")]
    allocation_size: Option<u64>,
    #[serde(default, rename = "objectClass")]
    object: Option<RawType>,
}

#[derive(Deserialize)]
struct RawThread {
    #[serde(default, rename = "javaName")]
    name: Option<String>,
    #[serde(default, rename = "osName")]
    os: Option<String>,
}

#[derive(Deserialize)]
struct RawStack {
    #[serde(default)]
    frames: Vec<RawFrame>,
}

#[derive(Deserialize)]
struct RawFrame {
    method: RawMethod,
    #[serde(default, rename = "lineNumber")]
    line_number: Option<i64>,
}

#[derive(Deserialize)]
struct RawMethod {
    #[serde(default)]
    name: String,
    #[serde(default)]
    descriptor: String,
    #[serde(default, rename = "type")]
    kind: RawType,
}

#[derive(Deserialize, Default)]
struct RawType {
    #[serde(default)]
    name: String,
    #[serde(default)]
    package: Option<RawPackage>,
}

#[derive(Deserialize)]
struct RawPackage {
    #[serde(default)]
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use project_model::{Module, Unit};

    const CPU: &str = r#"{
      "recording": {
        "events": [
          {"type": "jdk.ExecutionSample", "values": {
            "startTime": "2026-11-14T21:25:18.704+01:00",
            "sampledThread": {"javaName": "main"},
            "stackTrace": {"frames": [
            {"method": {"type": {"name": "demo.Counter", "package": {"name": "demo"}},
                        "name": "bump", "descriptor": "()I"}, "lineNumber": 7},
            {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                        "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 12}
          ]}}}
        ]
      }
    }"#;

    const ALLOCATIONS: &str = r#"{
      "recording": {
        "events": [
          {"type": "jdk.ObjectAllocationSample", "values": {
            "startTime": "2026-11-14T21:25:18.704+01:00",
            "eventThread": {"javaName": "main"},
            "objectClass": {"name": "[B", "package": null},
            "weight": 1258291,
            "stackTrace": {"frames": [
              {"method": {"type": {"name": "demo.Buffer", "package": {"name": "demo"}},
                          "name": "fill", "descriptor": "()V"}, "lineNumber": 4},
              {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                          "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 20}
            ]}}},
          {"type": "jdk.ObjectAllocationSample", "values": {
            "startTime": "2026-11-14T21:25:18.804+01:00",
            "eventThread": {"javaName": "main"},
            "objectClass": {"name": "[B", "package": null},
            "weight": 524288,
            "stackTrace": {"frames": [
              {"method": {"type": {"name": "demo.Buffer", "package": {"name": "demo"}},
                          "name": "fill", "descriptor": "()V"}, "lineNumber": 5},
              {"method": {"type": {"name": "demo.Main", "package": {"name": "demo"}},
                          "name": "main", "descriptor": "([Ljava/lang/String;)V"}, "lineNumber": 20}
            ]}}},
          {"type": "jdk.ObjectAllocationInNewTLAB", "values": {
            "startTime": "2026-11-14T21:25:18.906+01:00",
            "eventThread": {"javaName": "worker"},
            "objectClass": {"name": "demo/Box", "package": {"name": "demo"}},
            "allocationSize": 4096,
            "stackTrace": {"frames": [
              {"method": {"type": {"name": "demo.Buffer", "package": {"name": "demo"}},
                          "name": "fill", "descriptor": "()V"}, "lineNumber": 6}
            ]}}}
        ]
      }
    }"#;

    fn model(root: &Path) -> ProjectModel {
        ProjectModel {
            root: root.to_owned(),
            ..ProjectModel::default()
        }
    }

    #[test]
    fn the_tree_merges_equal_stacks_and_counts_own_and_total_samples() {
        let mut main = ProfileNode {
            frame: ProfileFrame {
                class: "demo.Main".into(),
                package: "demo".into(),
                method: "main".into(),
                signature: "([Ljava/lang/String;)V".into(),
                source: None,
                line: Some(12),
            },
            own: 0,
            total: 1,
            children: Vec::new(),
        };
        main.children.push(ProfileNode {
            frame: ProfileFrame {
                class: "demo.Counter".into(),
                package: "demo".into(),
                method: "bump".into(),
                signature: "()I".into(),
                source: None,
                line: Some(7),
            },
            own: 1,
            total: 1,
            children: Vec::new(),
        });
        let snapshot =
            parse_reader(CPU.as_bytes(), &model(Path::new("C:/p")), MAX_SAMPLES).unwrap();
        assert_eq!(snapshot.samples, 1);
        assert_eq!(snapshot.roots[0], main);
        assert_eq!(snapshot.roots[0].frame.line, Some(12));
        assert_eq!(snapshot.roots[0].children[0].frame.line, Some(7));
    }

    #[test]
    fn allocation_samples_of_both_events_fold_by_weight_and_make_the_types() {
        let report = parse_report(
            ALLOCATIONS.as_bytes(),
            &model(Path::new("C:/p")),
            MAX_SAMPLES,
        )
        .unwrap();

        assert!(report.has_allocations());
        let allocations = report.allocations.unwrap();
        assert_eq!(allocations.samples, 1_258_291 + 524_288 + 4096);
        let main = &allocations.roots[0];
        assert_eq!(main.frame.name(), "demo.Main.main");
        assert_eq!(main.total, 1_258_291 + 524_288);
        let fill = main.child("demo.Buffer.fill").unwrap();
        assert_eq!(
            (fill.own, fill.total),
            (1_258_291 + 524_288, 1_258_291 + 524_288)
        );

        let byte_array = &report.types[0];
        assert_eq!(byte_array.allocated.class, "[B");
        assert_eq!(byte_array.allocated.weight, 1_258_291 + 524_288);
        assert_eq!(byte_array.allocated.count, 2);
        assert!(
            byte_array.allocated.source.is_none(),
            "an array has no source"
        );
        assert_eq!(
            byte_array.tree.roots[0]
                .child("demo.Buffer.fill")
                .unwrap()
                .own,
            1_258_291 + 524_288
        );

        let boxed = &report.types[1];
        assert_eq!(boxed.allocated.class, "demo.Box");
        assert_eq!(boxed.allocated.package, "demo");
        assert_eq!(boxed.allocated.weight, 4096);
        assert_eq!(boxed.allocated.count, 1);
    }

    #[test]
    fn a_click_on_a_type_finds_the_tree_of_its_allocations() {
        let report = parse_report(
            ALLOCATIONS.as_bytes(),
            &model(Path::new("C:/p")),
            MAX_SAMPLES,
        )
        .unwrap();

        let tree = report.type_tree("demo.Box").unwrap();
        assert_eq!(tree.samples, 4096);
        assert_eq!(tree.roots[0].frame.name(), "demo.Buffer.fill");
        assert!(report.type_tree("[C").is_none());
    }

    #[test]
    fn the_cpu_samples_keep_their_moment_and_thread() {
        let json = r#"{"recording": {"events": [
            {"type": "jdk.ExecutionSample", "values": {
                "startTime": "2026-11-14T21:25:18.000+01:00",
                "sampledThread": {"javaName": "main"},
                "stackTrace": {"frames": [{"method": {"type": {"name": "demo.Main"},
                    "name": "main", "descriptor": "()V"}}]}}},
            {"type": "jdk.ExecutionSample", "values": {
                "startTime": "2026-11-14T21:25:18.250+01:00",
                "sampledThread": {"javaName": "worker"},
                "stackTrace": {"frames": [{"method": {"type": {"name": "demo.Main"},
                    "name": "main", "descriptor": "()V"}}]}}},
            {"type": "jdk.ExecutionSample", "values": {
                "startTime": "2026-11-14T21:25:18.500+01:00",
                "sampledThread": {"javaName": "main"},
                "stackTrace": {"frames": [{"method": {"type": {"name": "demo.Main"},
                    "name": "main", "descriptor": "()V"}}]}}}
        ]}}"#;

        let report = parse_report(json.as_bytes(), &model(Path::new("C:/p")), MAX_SAMPLES).unwrap();

        assert_eq!(
            report.cpu.lanes,
            vec!["main".to_owned(), "worker".to_owned()]
        );
        let times: Vec<u64> = report.cpu.samples.iter().map(|sample| sample.at).collect();
        assert_eq!(times, vec![0, 250, 500]);
        let lanes: Vec<u32> = report
            .cpu
            .samples
            .iter()
            .map(|sample| sample.lane)
            .collect();
        assert_eq!(lanes, vec![0, 1, 0]);
        assert_eq!(
            report.cpu.snapshot_between(Some((100, 400)), None).samples,
            1
        );
        assert_eq!(report.cpu.snapshot_between(None, Some(1)).samples, 1);
        assert_eq!(report.cpu.duration(), 501);
    }

    #[test]
    fn the_limit_counts_cpu_and_allocation_samples_together() {
        let json = r#"{"recording": {"events": [
                {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": [
                    {"method": {"type": {"name": "demo.Main"}, "name": "main", "descriptor": "()V"}}]}}},
                {"type": "jdk.ObjectAllocationSample", "values": {"weight": 100,
                    "objectClass": {"name": "[B"}, "stackTrace": {"frames": [
                    {"method": {"type": {"name": "demo.Main"}, "name": "main", "descriptor": "()V"}}]}}},
                {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": [
                    {"method": {"type": {"name": "demo.Main"}, "name": "main", "descriptor": "()V"}}]}}}
            ]}}"#;

        let report = parse_report(json.as_bytes(), &model(Path::new("C:/p")), 2).unwrap();

        assert_eq!(report.cpu_tree.samples, 1);
        assert!(report.cpu_tree.truncated);
        assert_eq!(report.allocations.unwrap().samples, 100);
    }

    #[test]
    fn an_event_without_a_stack_is_not_a_sample() {
        let json = r#"{"recording": {"events": [
            {"type": "jdk.ExecutionSample", "values": {}},
            {"type": "jdk.ExecutionSample", "values": {"stackTrace": {"frames": []}}},
            {"type": "jdk.ObjectAllocationSample", "values": {"weight": 10, "objectClass": {"name": "[B"}}}
        ]}}"#;

        let report = parse_report(json.as_bytes(), &model(Path::new("C:/p")), MAX_SAMPLES).unwrap();

        assert_eq!(report.cpu_tree.samples, 0);
        assert!(!report.has_allocations());
    }

    #[test]
    fn a_frame_of_the_project_gets_its_source_and_a_library_frame_does_not() {
        let root = test_support::scratch("profile-sources");
        let sources = root.join("src");
        fs::create_dir_all(sources.join("demo")).unwrap();
        fs::write(sources.join("demo/Main.java"), "class Main {}\n").unwrap();
        let model = ProjectModel {
            root: root.clone(),
            modules: vec![Module {
                units: vec![Unit {
                    source_roots: vec![sources.clone()],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        let snapshot = parse_reader(CPU.as_bytes(), &model, MAX_SAMPLES).unwrap();

        let frame = &snapshot.roots[0].frame;
        assert_eq!(frame.source, Some(sources.join("demo/Main.java")));
        assert!(frame.is_own());
        assert_eq!(frame.package, "demo");
    }

    #[test]
    fn the_recording_flag_names_the_file_and_writes_it_at_exit() {
        let flag = recording_flag(Path::new("C:/cache/profiler/App-1.jfr"));

        assert!(flag.starts_with("-XX:StartFlightRecording=filename=\""));
        assert!(flag.contains("App-1.jfr"));
        assert!(flag.contains("settings=profile"));
        assert!(flag.contains("dumponexit=true"));
    }

    #[test]
    fn the_files_of_a_run_share_its_name() {
        let root = test_support::scratch("profile-run-recordings");
        let directory = root.join(PROFILE_FOLDER);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Tests-77.jfr"), b"jfr").unwrap();
        fs::write(directory.join("Tests-77-2.jfr"), b"jfr").unwrap();
        fs::write(directory.join("Tests-78.jfr"), b"jfr").unwrap();

        let found = run_recordings(&directory.join("Tests-77.jfr"));

        assert_eq!(found.len(), 2, "both files of the same run");
        assert!(found.iter().all(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Tests-77")
        }));
    }

    #[test]
    fn only_the_newest_recordings_stay_in_the_folder() {
        let root = test_support::scratch("profile-recordings");
        let directory = root.join(PROFILE_FOLDER);
        fs::create_dir_all(&directory).unwrap();
        for index in 0..(MAX_RECORDINGS + 2) {
            fs::write(directory.join(format!("old-{index}.jfr")), b"jfr").unwrap();
        }

        let file = recording_file(&root, "My App").unwrap();

        assert_eq!(file.parent(), Some(directory.as_path()));
        assert!(
            file.file_stem()
                .unwrap()
                .to_string_lossy()
                .starts_with("My-App-")
        );
        fs::write(&file, b"jfr").unwrap();
        let left = fs::read_dir(&directory).unwrap().count();
        assert_eq!(left, MAX_RECORDINGS, "the folder keeps the newest only");
        assert_eq!(recordings(&root).len(), MAX_RECORDINGS);
    }

    #[test]
    fn a_jdk_8_before_the_update_has_no_flight_recorder() {
        let jdk = |feature: u32, version: &str| Jdk {
            home: PathBuf::from("C:/jdk"),
            version: version.to_owned(),
            feature,
        };

        assert!(!supported(&jdk(8, "1.8.0_252")));
        assert!(supported(&jdk(8, "1.8.0_492")));
        assert!(supported(&jdk(17, "17.0.20.1")));
    }

    #[test]
    fn a_time_with_an_offset_and_nanoseconds_reaches_the_epoch() {
        assert_eq!(
            parse_time("2026-11-14T21:25:18.704+01:00"),
            parse_time("2026-11-14T20:25:18.704Z")
        );
        assert_eq!(
            parse_time("2026-11-14T20:25:18.500Z").unwrap()
                - parse_time("2026-11-14T20:25:18.000Z").unwrap(),
            500
        );
    }
}
