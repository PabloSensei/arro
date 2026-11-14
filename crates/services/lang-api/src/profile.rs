//! What a profiler run sampled: stacks of frames with a weight each, folded into a tree
//! from the outermost frame down. The language brings the numbers and the source of a
//! frame of the project; the core knows nothing about how a run was recorded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One frame of a sampled stack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileFrame {
    /// The type, `demo.Counter`.
    pub class: String,
    /// The package part, `demo`, empty in the default package.
    pub package: String,
    pub method: String,
    /// The descriptor of the method, which tells overloads apart.
    pub signature: String,
    /// The source of a frame of the project, absent for the runtime and libraries.
    pub source: Option<PathBuf>,
    /// One-based line inside `source`.
    pub line: Option<u32>,
}

impl ProfileFrame {
    /// `demo.Counter.bump`, as a list shows it.
    pub fn name(&self) -> String {
        if self.class.is_empty() {
            self.method.clone()
        } else {
            format!("{}.{}", self.class, self.method)
        }
    }

    /// Whether the frame is code of the project, which the window colours brighter.
    pub fn is_own(&self) -> bool {
        self.source.is_some()
    }

    /// The package of a frame whose language gives no separate package.
    pub fn package_or_class(&self) -> &str {
        match self.package.is_empty() {
            true => self
                .class
                .rsplit_once('.')
                .map_or("", |(package, _)| package),
            false => &self.package,
        }
    }
}

/// A frame of the tree with the samples it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileNode {
    pub frame: ProfileFrame,
    /// The samples where this frame was the innermost one.
    pub own: u64,
    /// Every sample of this subtree, `own` included.
    pub total: u64,
    pub children: Vec<ProfileNode>,
}

impl ProfileNode {
    /// The child whose frame has `name`, for a test or a jump from a name.
    pub fn child(&self, name: &str) -> Option<&ProfileNode> {
        self.children
            .iter()
            .find(|child| child.frame.name() == name)
    }
}

/// One method over every place it appears in the tree, for the Method List.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileMethod {
    pub frame: ProfileFrame,
    /// The samples where the method was the innermost frame.
    pub own: u64,
    /// The samples of every subtree the method starts.
    pub total: u64,
}

/// The folded stacks of one recording.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileSnapshot {
    pub roots: Vec<ProfileNode>,
    /// The samples in the snapshot; the weights of the roots add up to it.
    pub samples: u64,
    /// Only the first samples were read, so the window shows a part of the run.
    pub truncated: bool,
}

impl ProfileSnapshot {
    /// The node at `path` below the roots.
    pub fn node(&self, path: &[usize]) -> Option<&ProfileNode> {
        let (first, rest) = path.split_first()?;
        let mut node = self.roots.get(*first)?;
        for index in rest {
            node = node.children.get(*index)?;
        }
        Some(node)
    }

    /// Every method of the tree with its own and total samples, the heaviest first.
    /// A method that calls itself counts at each level, as a profiler lists it.
    pub fn methods(&self) -> Vec<ProfileMethod> {
        let mut methods: Vec<ProfileMethod> = Vec::new();
        let mut at: HashMap<(String, String, String), usize> = HashMap::new();
        for root in &self.roots {
            collect(root, &mut methods, &mut at);
        }
        methods.sort_by(|left, right| {
            right
                .total
                .cmp(&left.total)
                .then_with(|| right.own.cmp(&left.own))
                .then_with(|| left.frame.name().cmp(&right.frame.name()))
        });
        methods
    }
}

fn collect(
    node: &ProfileNode,
    methods: &mut Vec<ProfileMethod>,
    at: &mut HashMap<(String, String, String), usize>,
) {
    let key = (
        node.frame.class.clone(),
        node.frame.method.clone(),
        node.frame.signature.clone(),
    );
    let index = match at.get(&key) {
        Some(index) => *index,
        None => {
            methods.push(ProfileMethod {
                frame: node.frame.clone(),
                own: 0,
                total: 0,
            });
            at.insert(key, methods.len() - 1);
            methods.len() - 1
        }
    };
    let method = &mut methods[index];
    method.own += node.own;
    method.total += node.total;
    if method.frame.source.is_none() {
        method.frame.source.clone_from(&node.frame.source);
        method.frame.line = node.frame.line;
    }
    for child in &node.children {
        collect(child, methods, at);
    }
}

/// How a profiler counts the weight of a stack: one per sample, or the bytes a sample
/// allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileUnit {
    /// One unit per recorded stack, as a CPU sampler gives.
    #[default]
    Samples,
    /// Bytes of the objects the stack allocated.
    Bytes,
}

impl ProfileUnit {
    /// The weight as the window writes it: `12` for samples, `1.2 MB` for bytes.
    pub fn text(self, weight: u64) -> String {
        match self {
            Self::Samples => weight.to_string(),
            Self::Bytes => bytes_text(weight),
        }
    }
}

/// `512 B`, `1.2 KB` or `34.5 MB` for a number of bytes.
pub fn bytes_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// One recorded stack with the moment it was taken, the thread it ran in and its weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSample {
    /// Milliseconds from the first sample of the recording.
    pub at: u64,
    /// The index of the thread in [`ProfileSamples::lanes`].
    pub lane: u32,
    pub weight: u64,
    /// Indices of the frames in [`ProfileSamples::frames`], outermost first.
    pub stack: Vec<u32>,
}

/// Every sample of one kind of a recording, kept apart from the folded tree so that the
/// window can show a timeline and fold the samples of an interval again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileSamples {
    /// The frames the stacks refer to by index.
    pub frames: Vec<ProfileFrame>,
    /// Thread names, one per lane, in the order they first appear.
    pub lanes: Vec<String>,
    pub samples: Vec<ProfileSample>,
    pub unit: ProfileUnit,
}

impl ProfileSamples {
    /// The weight of every sample added up.
    pub fn total(&self) -> u64 {
        self.samples.iter().map(|sample| sample.weight).sum()
    }

    /// Whether the sample falls into an interval from `from` to `to` milliseconds and,
    /// when given, into one thread.
    fn matches(sample: &ProfileSample, range: Option<(u64, u64)>, lane: Option<u32>) -> bool {
        if lane.is_some_and(|lane| lane != sample.lane) {
            return false;
        }
        range.is_none_or(|(from, to)| sample.at >= from && sample.at <= to)
    }

    /// The stacks of an interval and a thread folded into a tree. Frames of a stack that
    /// the samples share are folded once; the weights add up as they do for the whole
    /// recording.
    pub fn snapshot_between(
        &self,
        range: Option<(u64, u64)>,
        lane: Option<u32>,
    ) -> ProfileSnapshot {
        let mut roots: Vec<ProfileNode> = Vec::new();
        let mut total = 0;
        for sample in &self.samples {
            if !Self::matches(sample, range, lane) {
                continue;
            }
            let frames: Vec<&ProfileFrame> = sample
                .stack
                .iter()
                .filter_map(|index| self.frames.get(*index as usize))
                .collect();
            if frames.is_empty() {
                continue;
            }
            total += sample.weight;
            insert_weighted(&mut roots, &frames, 0, sample.weight);
        }
        ProfileSnapshot {
            roots,
            samples: total,
            truncated: false,
        }
    }

    /// The whole recording folded into a tree.
    pub fn snapshot(&self) -> ProfileSnapshot {
        self.snapshot_between(None, None)
    }

    /// The length of the recording in milliseconds, at least one when it has a sample.
    pub fn duration(&self) -> u64 {
        let last = self
            .samples
            .iter()
            .map(|sample| sample.at)
            .max()
            .unwrap_or(0);
        last + u64::from(!self.samples.is_empty())
    }
}

fn insert_weighted(nodes: &mut Vec<ProfileNode>, stack: &[&ProfileFrame], at: usize, weight: u64) {
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
    insert_weighted(&mut node.children, stack, at + 1, weight);
}

fn same_frame(one: &ProfileFrame, other: &ProfileFrame) -> bool {
    one.class == other.class && one.method == other.method && one.signature == other.signature
}

/// The objects of one class that the recording allocated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileType {
    /// The class, `demo.Buffer` or `[B` for a byte array.
    pub class: String,
    pub package: String,
    /// The source of a class of the project.
    pub source: Option<PathBuf>,
    /// The bytes of every allocation sample of the class.
    pub weight: u64,
    /// How many allocation samples the class had.
    pub count: u64,
}

/// The allocations of one class folded into a tree, for the trees of a click on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileTypeTree {
    pub allocated: ProfileType,
    pub tree: ProfileSnapshot,
}

/// What one recording holds: the CPU samples with the timeline of their threads, the
/// allocations and the classes they made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileReport {
    /// The samples of the CPU with their moment and thread.
    pub cpu: ProfileSamples,
    /// The CPU samples folded into a tree.
    pub cpu_tree: ProfileSnapshot,
    /// The allocation samples folded into a tree by weight in bytes, absent when the
    /// recording has none.
    pub allocations: Option<ProfileSnapshot>,
    /// The classes the recording allocated, the heaviest first.
    pub types: Vec<ProfileTypeTree>,
}

impl ProfileReport {
    /// A report of a CPU tree alone, for a provider that gives no allocations or a test
    /// that knows the tree in advance.
    pub fn of(snapshot: ProfileSnapshot) -> Self {
        Self {
            cpu: ProfileSamples::default(),
            cpu_tree: snapshot,
            allocations: None,
            types: Vec::new(),
        }
    }

    /// Whether the recording has allocation samples.
    pub fn has_allocations(&self) -> bool {
        self.allocations
            .as_ref()
            .is_some_and(|tree| tree.samples > 0)
    }

    /// The tree of the allocated class, for a click on Allocated Types.
    pub fn type_tree(&self, class: &str) -> Option<&ProfileSnapshot> {
        self.types
            .iter()
            .find(|entry| entry.allocated.class == class)
            .map(|entry| &entry.tree)
    }
}

/// A recording the profiler can open again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRecording {
    /// The file of the recording.
    pub path: PathBuf,
    /// What the list shows: the name the provider gives the file.
    pub label: String,
}

impl ProfileRecording {
    /// A recording named after its file, for a provider that has nothing better.
    pub fn named(path: PathBuf) -> Self {
        let label = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        Self { path, label }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(class: &str, method: &str) -> ProfileFrame {
        ProfileFrame {
            class: class.to_owned(),
            package: class
                .rsplit_once('.')
                .map_or("", |(package, _)| package)
                .to_owned(),
            method: method.to_owned(),
            signature: "()V".to_owned(),
            source: None,
            line: None,
        }
    }

    fn node(frame: ProfileFrame, own: u64, total: u64) -> ProfileNode {
        ProfileNode {
            frame,
            own,
            total,
            children: Vec::new(),
        }
    }

    #[test]
    fn the_name_is_the_class_and_the_method() {
        assert_eq!(frame("demo.Counter", "bump").name(), "demo.Counter.bump");
        assert_eq!(
            frame("", "bump").name(),
            "bump",
            "a frame without a class is a function of a script"
        );
    }

    #[test]
    fn the_methods_add_up_every_place_of_the_tree() {
        let mut main = node(frame("demo.Main", "main"), 0, 5);
        let mut work = node(frame("demo.Main", "work"), 3, 4);
        work.children = vec![node(frame("lib.String", "length"), 1, 1)];
        main.children = vec![work];
        let snapshot = ProfileSnapshot {
            roots: vec![main],
            samples: 5,
            truncated: false,
        };

        let methods = snapshot.methods();

        assert_eq!(methods[0].frame.name(), "demo.Main.main");
        assert_eq!((methods[0].own, methods[0].total), (0, 5));
        assert_eq!(methods[1].frame.name(), "demo.Main.work");
        assert_eq!((methods[1].own, methods[1].total), (3, 4));
        assert_eq!(methods[2].frame.name(), "lib.String.length");
    }

    #[test]
    fn a_node_is_found_by_its_path_below_the_roots() {
        let mut main = node(frame("demo.Main", "main"), 0, 2);
        main.children = vec![node(frame("demo.Main", "work"), 2, 2)];
        let snapshot = ProfileSnapshot {
            roots: vec![main],
            samples: 2,
            truncated: false,
        };

        assert_eq!(
            snapshot.node(&[0, 0]).map(|node| node.frame.name()),
            Some("demo.Main.work".to_owned())
        );
        assert!(snapshot.node(&[0, 1]).is_none());
        assert!(snapshot.node(&[]).is_none());
    }

    #[test]
    fn weights_read_as_bytes_or_as_samples() {
        assert_eq!(bytes_text(0), "0 B");
        assert_eq!(bytes_text(512), "512 B");
        assert_eq!(bytes_text(1024), "1.0 KB");
        assert_eq!(bytes_text(1_258_291), "1.2 MB");
        assert_eq!(bytes_text(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(ProfileUnit::Samples.text(12), "12");
        assert_eq!(ProfileUnit::Bytes.text(1_258_291), "1.2 MB");
    }

    #[test]
    fn the_samples_of_an_interval_and_a_thread_fold_into_their_own_tree() {
        let mut samples = ProfileSamples {
            frames: vec![frame("demo.Main", "main"), frame("demo.Heap", "grow")],
            lanes: vec!["main".into(), "worker".into()],
            samples: Vec::new(),
            unit: ProfileUnit::Bytes,
        };
        samples.samples = vec![
            ProfileSample {
                at: 10,
                lane: 0,
                weight: 100,
                stack: vec![0, 1],
            },
            ProfileSample {
                at: 20,
                lane: 0,
                weight: 300,
                stack: vec![0, 1],
            },
            ProfileSample {
                at: 15,
                lane: 1,
                weight: 50,
                stack: vec![0],
            },
        ];

        let all = samples.snapshot();
        assert_eq!(all.samples, 450);
        assert_eq!(all.roots[0].total, 450);
        assert_eq!(all.roots[0].child("demo.Heap.grow").unwrap().own, 400);

        let interval = samples.snapshot_between(Some((15, 20)), None);
        assert_eq!(
            interval.samples, 350,
            "the sample before the interval is out"
        );
        assert_eq!(interval.roots[0].child("demo.Heap.grow").unwrap().own, 300);

        let worker = samples.snapshot_between(None, Some(1));
        assert_eq!(worker.samples, 50);
        assert_eq!(worker.roots[0].frame.name(), "demo.Main.main");

        assert_eq!(samples.duration(), 21);
        assert_eq!(samples.total(), 450);
    }

    #[test]
    fn a_report_of_a_tree_has_no_allocations_until_it_is_given_them() {
        let snapshot = ProfileSnapshot {
            roots: vec![node(frame("demo.Main", "main"), 1, 1)],
            samples: 1,
            truncated: false,
        };
        let report = ProfileReport::of(snapshot.clone());

        assert_eq!(report.cpu_tree, snapshot);
        assert!(!report.has_allocations());
        assert!(report.type_tree("demo.Buffer").is_none());
    }

    #[test]
    fn a_recording_is_named_after_its_file() {
        let recording = ProfileRecording::named(PathBuf::from("C:/cache/profiler/App-1.rec"));

        assert_eq!(recording.label, "App-1.rec");
        assert_eq!(recording.path(), Path::new("C:/cache/profiler/App-1.rec"));
    }
}
