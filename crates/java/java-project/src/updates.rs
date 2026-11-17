//! Newer versions of a dependency: the order Maven gives to version numbers, the list a
//! repository publishes in `maven-metadata.xml`, the cache of those lists, and the places
//! a build file names the version of an artifact.
//!
//! Reading runs on the caller's thread and never waits for the interface: the version
//! source is asked from a background thread, and an answer that takes too long simply
//! leaves the build file without a hint. The cache lives under the cache folder of the
//! IDE for a day, so a second open of the same project asks nothing of the network.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};

use crate::sources::{MAVEN_CENTRAL, repositories_of};

/// How long a list of versions from a repository stays good.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The folder of the IDE cache that holds the lists of versions.
pub const CACHE_FOLDER: &str = "dependency-updates";

/// How many requests to repositories run at the same time.
const CONCURRENCY: usize = 4;

/// How long one repository has to answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Which build file a text is, as far as the version places go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildFile {
    /// `pom.xml`: `<dependency>` and `<plugin>` elements, and the properties they use.
    Pom,
    /// `build.gradle` and `build.gradle.kts`.
    Script,
    /// `libs.versions.toml`.
    Catalog,
}

/// The build file a path names, or `None` for every other file.
pub fn kind_of(path: &Path) -> Option<BuildFile> {
    let name = path.file_name()?.to_string_lossy();
    match name.as_ref() {
        "pom.xml" => Some(BuildFile::Pom),
        "build.gradle" | "build.gradle.kts" => Some(BuildFile::Script),
        "libs.versions.toml" => Some(BuildFile::Catalog),
        _ => None,
    }
}

/// One place a build file names the version of an artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub group: String,
    pub artifact: String,
    /// The version as written, empty where a version is only being typed.
    pub version: String,
    /// The bytes of the version to replace with another one.
    pub range: Range<usize>,
}

/// Every place of a build file where a version is written, including an empty version
/// element and a value typed into a repository coordinate, for completion.
pub fn slots(file: BuildFile, text: &str) -> Vec<Slot> {
    let mut found = match file {
        BuildFile::Pom => pom_slots(text),
        BuildFile::Script => script_slots(text),
        BuildFile::Catalog => catalog_slots(text),
    };
    found.sort_by_key(|slot| slot.range.start);
    found
}

/// The places a hint may go: a version exists and names no range, no prefix and no
/// property of another file. A place that two artifacts share is left out.
pub fn sites(file: BuildFile, text: &str) -> Vec<Slot> {
    let found: Vec<Slot> = slots(file, text)
        .into_iter()
        .filter(|slot| is_plain_version(&slot.version))
        .collect();
    unambiguous(found)
}

/// Keeps one place per byte range, and only when every artifact of that place is the
/// same one: a property of two dependencies says nothing about either of them.
pub fn unambiguous(slots: Vec<Slot>) -> Vec<Slot> {
    let mut by_range: BTreeMap<(usize, usize), Vec<Slot>> = BTreeMap::new();
    for slot in slots {
        by_range
            .entry((slot.range.start, slot.range.end))
            .or_default()
            .push(slot);
    }
    by_range
        .into_values()
        .filter_map(|found| {
            let first = found.first()?.clone();
            found
                .iter()
                .all(|slot| slot.group == first.group && slot.artifact == first.artifact)
                .then_some(first)
        })
        .collect()
}

/// The place of `text` a caret at `offset` stands in, for completion and for the fix of
/// the version under it.
pub fn slot_at(file: BuildFile, text: &str, offset: usize) -> Option<Slot> {
    unambiguous(slots(file, text))
        .into_iter()
        .find(|slot| slot.range.start <= offset && offset <= slot.range.end)
}

fn is_plain_version(version: &str) -> bool {
    !version.is_empty()
        && !version.contains(['$', '+', '[', ']', '(', ')', ',', ' ', '*'])
        && version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

/// One artifact name and the version the project asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub group: String,
    pub artifact: String,
    pub current: String,
}

/// What a hint offers: the newest stable version, and the newest stable version of the
/// same `major.minor`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Updates {
    pub stable: Option<String>,
    pub patch: Option<String>,
}

impl Updates {
    pub fn is_empty(&self) -> bool {
        self.stable.is_none() && self.patch.is_none()
    }
}

/// The offers of `versions` for an artifact whose version is `current`. A pre-release is
/// never offered over a stable version.
pub fn updates_for(current: &str, versions: &[String]) -> Updates {
    let stable: Vec<&String> = versions
        .iter()
        .filter(|it| !is_prerelease(it.as_str()))
        .collect();
    let newest = stable
        .iter()
        .map(|it| (it.as_str(), Version::new(it.as_str())))
        .filter(|(_, version)| *version > Version::new(current))
        .max_by(|left, right| left.1.cmp(&right.1))
        .map(|(text, _)| text.to_owned());
    let patch = stable
        .iter()
        .filter(|it| same_minor(current, it.as_str()))
        .map(|it| (it.as_str(), Version::new(it.as_str())))
        .filter(|(_, version)| *version > Version::new(current))
        .max_by(|left, right| left.1.cmp(&right.1))
        .map(|(text, _)| text.to_owned());
    let patch = patch.filter(|patch| Some(patch) != newest.as_ref());
    Updates {
        stable: newest,
        patch,
    }
}

fn same_minor(left: &str, right: &str) -> bool {
    let left = numeric_prefix(left);
    let right = numeric_prefix(right);
    left.len() >= 2 && right.len() >= 2 && left[0] == right[0] && left[1] == right[1]
}

fn numeric_prefix(version: &str) -> Vec<u64> {
    let mut found = Vec::new();
    for part in version.split(['.', '-', '_']) {
        match part.parse::<u64>() {
            Ok(value) => found.push(value),
            Err(_) => {
                if !found.is_empty() {
                    break;
                }
            }
        }
    }
    found
}

/// Whether Maven sorts `version` before the release of the same numbers.
pub fn is_prerelease(version: &str) -> bool {
    const QUALIFIERS: [&str; 9] = [
        "alpha",
        "a",
        "beta",
        "b",
        "milestone",
        "m",
        "rc",
        "cr",
        "snapshot",
    ];
    Version::new(version).is_prerelease(&QUALIFIERS)
}

/// A version compared the way Maven compares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    pub fn new(text: &str) -> Self {
        Self(text.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn items(&self) -> Vec<Item> {
        parse_items(&self.0)
    }

    fn is_prerelease(&self, qualifiers: &[&str]) -> bool {
        fn token_is_prerelease(token: &str, qualifiers: &[&str]) -> bool {
            let token = token.trim_start_matches(|c: char| c.is_ascii_digit());
            qualifiers.iter().any(|qualifier| {
                token
                    .strip_prefix(qualifier)
                    .is_some_and(|rest| rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()))
            })
        }
        let lowered = self.0.to_ascii_lowercase();
        lowered
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .any(|token| token_is_prerelease(token, qualifiers))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_items(&self.items(), &other.items())
    }
}

/// The order of two versions, as Maven's `ComparableVersion` gives it: numbers over
/// qualifiers, the qualifiers `alpha < beta < milestone < rc < snapshot < release < sp`,
/// an unknown qualifier after every known one, and trailing zeros that mean nothing.
pub fn compare(left: &str, right: &str) -> Ordering {
    compare_items(&parse_items(left), &parse_items(right))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Number(u64),
    Text(String),
    List(Vec<Item>),
}

fn compare_items(left: &[Item], right: &[Item]) -> Ordering {
    let count = left.len().max(right.len());
    for index in 0..count {
        let order = compare_item(left.get(index), right.get(index));
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn compare_item(left: Option<&Item>, right: Option<&Item>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_both(left, right),
        (Some(left), None) => compare_to_null(left),
        (None, Some(right)) => compare_to_null(right).reverse(),
        (None, None) => Ordering::Equal,
    }
}

fn compare_both(left: &Item, right: &Item) -> Ordering {
    match (left, right) {
        (Item::Number(left), Item::Number(right)) => left.cmp(right),
        (Item::Text(left), Item::Text(right)) => qualifier(left).cmp(&qualifier(right)),
        (Item::List(left), Item::List(right)) => compare_items(left, right),
        // A list of a hyphen sits below both a number and a word, a word below a number.
        (Item::List(_), _) => Ordering::Less,
        (_, Item::List(_)) => Ordering::Greater,
        (Item::Text(_), Item::Number(_)) => Ordering::Less,
        (Item::Number(_), Item::Text(_)) => Ordering::Greater,
    }
}

fn compare_to_null(item: &Item) -> Ordering {
    match item {
        Item::Number(value) if *value == 0 => Ordering::Equal,
        Item::Number(_) => Ordering::Greater,
        Item::Text(text) => qualifier(text).cmp(&qualifier("")),
        // `1.0-rc1` is older than `1.0`: the first item of the list decides, not the last.
        Item::List(items) => items
            .iter()
            .map(compare_to_null)
            .find(|order| *order != Ordering::Equal)
            .unwrap_or(Ordering::Equal),
    }
}

/// The order Maven gives a qualifier: the known ones by rank, every other after them, and
/// the abbreviations of the known ones as the words themselves.
fn qualifier(text: &str) -> (u8, String) {
    let alias = match text {
        "a" => "alpha",
        "b" => "beta",
        "m" => "milestone",
        "cr" => "rc",
        "ga" | "final" | "release" => "",
        other => other,
    };
    match alias {
        "alpha" => (0, String::new()),
        "beta" => (1, String::new()),
        "milestone" => (2, String::new()),
        "rc" => (3, String::new()),
        "snapshot" => (4, String::new()),
        "" => (5, String::new()),
        "sp" => (6, String::new()),
        other => (7, other.to_owned()),
    }
}

fn parse_items(version: &str) -> Vec<Item> {
    let text = version.to_ascii_lowercase();
    let mut stack: Vec<Vec<Item>> = vec![Vec::new()];
    let mut start = 0;
    let mut digits = false;
    for (index, character) in text.char_indices() {
        if character.is_ascii_digit() {
            if !digits && index > start {
                push(&mut stack, Some(Item::Text(text[start..index].to_owned())));
                start = index;
            }
            digits = true;
        } else if character == '.' {
            push(&mut stack, item_of(&text[start..index], digits));
            start = index + 1;
        } else if character == '-' {
            push(&mut stack, item_of(&text[start..index], digits));
            stack.push(Vec::new());
            start = index + 1;
        } else if digits && index > start {
            push(&mut stack, item_of(&text[start..index], digits));
            start = index;
            digits = false;
        } else {
            digits = false;
        }
    }
    if text.len() > start {
        push(&mut stack, item_of(&text[start..], digits));
    }
    while stack.len() > 1 {
        let sub = trim(stack.pop().unwrap_or_default());
        let parent = stack.last_mut().expect("the root stays");
        parent.push(Item::List(sub));
    }
    trim(stack.pop().unwrap_or_default())
}

fn push(stack: &mut [Vec<Item>], item: Option<Item>) {
    if let (Some(item), Some(list)) = (item, stack.last_mut()) {
        list.push(item);
    }
}

fn item_of(text: &str, digits: bool) -> Option<Item> {
    if text.is_empty() {
        return Some(Item::Number(0));
    }
    if digits {
        Some(Item::Number(text.parse().unwrap_or(0)))
    } else {
        Some(Item::Text(text.to_owned()))
    }
}

/// Drops the trailing zeros and empty words of a list, as Maven does.
fn trim(mut items: Vec<Item>) -> Vec<Item> {
    while let Some(last) = items.last() {
        let null = match last {
            Item::Number(value) => *value == 0,
            Item::Text(text) => text.is_empty(),
            Item::List(items) => items.is_empty(),
        };
        if !null {
            break;
        }
        items.pop();
    }
    items
}

/// The list of versions a repository publishes for one artifact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub latest: Option<String>,
    pub release: Option<String>,
    pub versions: Vec<String>,
}

/// Reads `maven-metadata.xml`. A broken file has no versions and no error: the interface
/// shows no hint instead of a message the user cannot act on.
pub fn parse_metadata(xml: &str) -> Metadata {
    let Ok(document) = Document::parse(xml) else {
        return Metadata::default();
    };
    let root = document.root_element();
    let versioning = root.descendants().find(|node| {
        node.is_element()
            && node.tag_name().name() == "versioning"
            && node
                .ancestors()
                .any(|parent| parent.is_element() && parent.tag_name().name() == "metadata")
    });
    let Some(versioning) = versioning else {
        return Metadata::default();
    };
    let text = |name: &str| {
        versioning
            .children()
            .find(|child| child.is_element() && child.tag_name().name() == name)
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let versions = versioning
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "versions")
        .map(|node| {
            node.children()
                .filter(|child| child.is_element() && child.tag_name().name() == "version")
                .filter_map(|node| node.text())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Metadata {
        latest: text("latest"),
        release: text("release"),
        versions,
    }
}

/// The versions of an artifact, from the memory of this process, from the disk of the
/// IDE, or from a repository. `now` is the number of seconds since the epoch.
#[derive(Debug)]
pub struct VersionSource {
    cache: PathBuf,
    memory: Mutex<HashMap<String, (Vec<String>, u64)>>,
    gate: Gate,
}

impl VersionSource {
    pub fn new(cache: PathBuf) -> Self {
        Self {
            cache,
            memory: Mutex::new(HashMap::new()),
            gate: Gate::new(CONCURRENCY),
        }
    }

    /// The versions of the cache while they are younger than [`TTL`], without any wait.
    pub fn cached(&self, group: &str, artifact: &str, now: u64) -> Option<Vec<String>> {
        let key = format!("{group}:{artifact}");
        if let Some((versions, fetched)) = self
            .memory
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&key)
            && now.saturating_sub(*fetched) < TTL.as_secs()
        {
            return Some(versions.clone());
        }
        let file = cache_file(&self.cache, group, artifact)?;
        let versions = read_cache(&file, now)?;
        self.remember(group, artifact, &versions, now, false);
        Some(versions)
    }

    /// The versions of an artifact, oldest first. Blocks on the network, so the caller is
    /// a thread of the language and never the interface.
    pub fn versions(
        &self,
        group: &str,
        artifact: &str,
        repositories: &[String],
        now: u64,
    ) -> Option<Vec<String>> {
        if let Some(versions) = self.cached(group, artifact, now) {
            return Some(versions);
        }
        if repositories.is_empty() {
            return None;
        }
        let _slot = self.gate.acquire();
        // Another thread may have filled the cache while this one waited for its slot.
        if let Some(versions) = self.cached(group, artifact, now) {
            return Some(versions);
        }
        for repository in repositories {
            let Some(metadata) = fetch_metadata(repository, group, artifact) else {
                continue;
            };
            if metadata.versions.is_empty() {
                continue;
            }
            let versions = sorted(&metadata.versions);
            self.remember(group, artifact, &versions, now, true);
            return Some(versions);
        }
        None
    }

    fn remember(&self, group: &str, artifact: &str, versions: &[String], now: u64, disk: bool) {
        let key = format!("{group}:{artifact}");
        self.memory
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(key, (versions.to_vec(), now));
        if disk && let Some(file) = cache_file(&self.cache, group, artifact) {
            let _ = write_cache(&file, versions, now);
        }
    }
}

/// Versions from the oldest to the newest, duplicates dropped.
pub fn sorted(versions: &[String]) -> Vec<String> {
    let mut sorted: Vec<String> = versions.to_vec();
    sorted.sort_by(|left, right| compare(left, right));
    sorted.dedup();
    sorted
}

/// Where the versions of one artifact are cached. Only the characters a name may carry
/// build a path, so a build file cannot name a file outside the cache.
pub fn cache_file(cache: &Path, group: &str, artifact: &str) -> Option<PathBuf> {
    let mut path = cache.to_path_buf();
    for part in group.split('.') {
        if !is_name_part(part) {
            return None;
        }
        path.push(part);
    }
    if !is_name_part(artifact) {
        return None;
    }
    path.push(format!("{artifact}.json"));
    Some(path)
}

fn is_name_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

#[derive(Debug, Deserialize, Serialize)]
struct Cached {
    fetched: u64,
    versions: Vec<String>,
}

/// The cached versions while they are younger than [`TTL`].
pub fn read_cache(file: &Path, now: u64) -> Option<Vec<String>> {
    let text = fs::read_to_string(file).ok()?;
    let cached: Cached = serde_json::from_str(&text).ok()?;
    (now.saturating_sub(cached.fetched) < TTL.as_secs()).then_some(cached.versions)
}

/// Writes the versions beside their place first, so a stop leaves no half a file behind.
pub fn write_cache(file: &Path, versions: &[String], now: u64) -> std::io::Result<()> {
    let Some(parent) = file.parent() else {
        return Err(std::io::Error::other("the cache file has no folder"));
    };
    fs::create_dir_all(parent)?;
    let cached = Cached {
        fetched: now,
        versions: versions.to_vec(),
    };
    let text = serde_json::to_string(&cached)?;
    let part = file.with_extension("json.part");
    fs::write(&part, text)?;
    fs::rename(&part, file).inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })
}

/// The number of seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The addresses to ask for the versions of a build file in `root`. A repository that
/// names credentials or a property is left out: the IDE has no way to fill either in
/// without a message about something the user did not ask for.
pub fn repositories(root: &Path) -> Vec<String> {
    let urls = repositories_of(root, &[]);
    let mut found = Vec::new();
    for url in urls {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://"))
            || url.contains('@')
            || url.contains("${")
        {
            continue;
        }
        let url = url.trim_end_matches('/');
        if !found.iter().any(|known: &String| known == url) {
            found.push(url.to_owned());
        }
    }
    // The super POM of a project without a central repository still names Central.
    if !found.iter().any(|url| url == MAVEN_CENTRAL) {
        found.push(MAVEN_CENTRAL.to_owned());
    }
    found
}

/// The order the versions of a repository are offered in.
pub fn newest_first(versions: &[String]) -> Vec<String> {
    let mut versions = versions.to_vec();
    versions.sort_by(|left, right| compare(right, left));
    versions.dedup();
    versions
}

fn fetch_metadata(repository: &str, group: &str, artifact: &str) -> Option<Metadata> {
    let group_path = group.replace('.', "/");
    let url = format!(
        "{}/{group_path}/{artifact}/maven-metadata.xml",
        repository.trim_end_matches('/')
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .timeout_connect(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(5)
        .max_redirects_will_error(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let response = agent.get(&url).call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut body = String::new();
    let mut reader = response.into_body().into_reader().take(4 * 1024 * 1024);
    reader.read_to_string(&mut body).ok()?;
    Some(parse_metadata(&body))
}

/// The number of requests to repositories that may run at once.
#[derive(Debug)]
struct Gate {
    limit: usize,
    free: Mutex<usize>,
    ready: Condvar,
}

impl Gate {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            free: Mutex::new(limit),
            ready: Condvar::new(),
        }
    }

    fn acquire(&self) -> GateSlot<'_> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while *free == 0 {
            free = self
                .ready
                .wait(free)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        *free -= 1;
        GateSlot { gate: self }
    }
}

struct GateSlot<'a> {
    gate: &'a Gate,
}

impl Drop for GateSlot<'_> {
    fn drop(&mut self) {
        let mut free = self
            .gate
            .free
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *free = (*free + 1).min(self.gate.limit);
        self.gate.ready.notify_one();
    }
}

trait LocalName {
    fn has_name(&self, name: &str) -> bool;
}

impl LocalName for Node<'_, '_> {
    fn has_name(&self, name: &str) -> bool {
        self.is_element() && self.tag_name().name() == name
    }
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_name(name))
}

fn text_of(node: Node, name: &str) -> Option<String> {
    child(node, name)?
        .text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The text of a node with the bytes of that text in the source.
fn value_of(text: &str, node: Node) -> Option<(String, Range<usize>)> {
    let child = node.children().find(|child| child.is_text())?;
    let range = child.range();
    let raw = &text[range.clone()];
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = range.start + (raw.len() - raw.trim_start().len());
    let end = start + trimmed.len();
    Some((trimmed.to_owned(), start..end))
}

/// Where the text of an empty element goes, so that completion can fill it.
fn empty_value_range(text: &str, node: Node) -> Option<Range<usize>> {
    if node.children().any(|child| child.is_text()) {
        return None;
    }
    let range = node.range();
    let head = &text[range.clone()];
    let at = head
        .find("/>")
        .map(|found| range.start + found)
        .or_else(|| head.find('>').map(|found| range.start + found + 1))?;
    Some(at..at)
}

fn property_of(value: &str) -> Option<&str> {
    value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn pom_slots(text: &str) -> Vec<Slot> {
    let Ok(document) = Document::parse(text) else {
        return Vec::new();
    };
    let root = document.root_element();
    let mut properties: HashMap<String, (String, Range<usize>)> = HashMap::new();
    if let Some(node) = child(root, "properties") {
        for property in node.children().filter(Node::is_element) {
            if let Some((value, range)) = value_of(text, property) {
                properties.insert(property.tag_name().name().to_owned(), (value, range));
            }
        }
    }
    let mut slots = Vec::new();
    for node in root.descendants().filter(Node::is_element) {
        let default_group = match node.tag_name().name() {
            "dependency" => None,
            "plugin" => Some("org.apache.maven.plugins"),
            _ => continue,
        };
        let Some(artifact) = text_of(node, "artifactId") else {
            continue;
        };
        let group = text_of(node, "groupId")
            .or_else(|| default_group.map(str::to_owned))
            .unwrap_or_default();
        if group.is_empty() {
            continue;
        }
        let slot = |version: String, range: Range<usize>| Slot {
            group: group.clone(),
            artifact: artifact.clone(),
            version,
            range,
        };
        let Some(version) = child(node, "version") else {
            continue;
        };
        match value_of(text, version) {
            Some((raw, range)) => match property_of(&raw) {
                Some(name) => {
                    if let Some((value, value_range)) = properties.get(name) {
                        slots.push(slot(value.clone(), value_range.clone()));
                    }
                }
                None => slots.push(slot(raw, range)),
            },
            None => {
                if let Some(range) = empty_value_range(text, version) {
                    slots.push(slot(String::new(), range));
                }
            }
        }
    }
    slots
}

fn script_slots(text: &str) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut quote: Option<(char, usize)> = None;
    for (index, character) in text.char_indices() {
        match quote {
            None if character == '"' || character == '\'' => {
                quote = Some((character, index + character.len_utf8()));
            }
            Some((opening, start)) if character == opening => {
                if let Some(slot) = coordinate_slot(start, &text[start..index]) {
                    slots.push(slot);
                }
                quote = None;
            }
            _ => {}
        }
    }
    slots.extend(named_script_slots(text));
    slots
}

/// `"group:artifact:version"` in either script language.
fn coordinate_slot(base: usize, content: &str) -> Option<Slot> {
    if content.contains(['$', ' ', '\t']) {
        return None;
    }
    let first = content.find(':')?;
    let second = first + 1 + content[first + 1..].find(':')?;
    let group = &content[..first];
    let artifact = &content[first + 1..second];
    if !is_coordinate_name(group) || !is_coordinate_name(artifact) {
        return None;
    }
    let version_start = second + 1;
    let version_end = content[version_start..]
        .find(':')
        .map(|found| version_start + found)
        .unwrap_or(content.len());
    let version = &content[version_start..version_end];
    if !version.is_empty() && !is_plain_version(version) {
        return None;
    }
    Some(Slot {
        group: group.to_owned(),
        artifact: artifact.to_owned(),
        version: version.to_owned(),
        range: base + version_start..base + version_end,
    })
}

fn is_coordinate_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

/// `group: 'g', name: 'a', version: 'v'` and the Kotlin spelling with `=`.
fn named_script_slots(text: &str) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let start = line_start;
        line_start += line.len();
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let Some((version, range)) = named_string(line, "version") else {
            continue;
        };
        let Some((group, _)) = named_string(line, "group") else {
            continue;
        };
        let Some((artifact, _)) = named_string(line, "name") else {
            continue;
        };
        if !is_coordinate_name(&group) || !is_coordinate_name(&artifact) {
            continue;
        }
        if !version.is_empty() && !is_plain_version(&version) {
            continue;
        }
        slots.push(Slot {
            group,
            artifact,
            version,
            range: start + range.start..start + range.end,
        });
    }
    slots
}

/// The quoted value of `keyword: 'value'` or `keyword = "value"` in one line.
fn named_string(line: &str, keyword: &str) -> Option<(String, Range<usize>)> {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut from = 0;
    while let Some(found) = line[from..].find(keyword) {
        let at = from + found;
        from = at + keyword.len();
        if line[..at].chars().next_back().is_some_and(is_word)
            || line[from..].chars().next().is_some_and(is_word)
        {
            continue;
        }
        let rest = &line[from..];
        let spaced = rest.trim_start();
        let spaces = rest.len() - spaced.len();
        let mut characters = spaced.char_indices();
        if !matches!(characters.next(), Some((_, ':' | '='))) {
            continue;
        }
        let after: &str = &spaced[1..];
        let value = after.trim_start();
        let inner = after.len() - value.len();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let start = from + spaces + 1 + inner + 1;
        let end = start + line[start..].find(quote)?;
        return Some((line[start..end].to_owned(), start..end));
    }
    None
}

/// One entry of `[libraries]`: what it names and where its version stands.
struct CatalogLibrary {
    group: Option<String>,
    artifact: Option<String>,
    reference: Option<String>,
    inline: Option<(String, Range<usize>)>,
}

fn catalog_slots(text: &str) -> Vec<Slot> {
    let mut versions: HashMap<String, (String, Range<usize>)> = HashMap::new();
    let mut libraries: Vec<CatalogLibrary> = Vec::new();
    let mut section = "";
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let start = line_start;
        line_start += line.len();
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.trim_matches(['[', ']']);
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(equal) = line.find('=') else {
            continue;
        };
        let key = line[..equal].trim();
        if key.is_empty() || key.contains([' ', '"', '\'']) {
            continue;
        }
        let after = &line[equal + 1..];
        let value = after.trim_start();
        let value_start = equal + 1 + (after.len() - value.len());
        match section {
            "versions" => {
                if let Some((value, range)) = quoted_at(line, value_start) {
                    versions.insert(
                        key.to_owned(),
                        (value, start + range.start..start + range.end),
                    );
                }
            }
            "libraries" => {
                let module = named_string(value, "module").and_then(|(found, _)| {
                    let (group, artifact) = found.split_once(':')?;
                    Some((group.to_owned(), artifact.to_owned()))
                });
                let group = named_string(value, "group").map(|(found, _)| found);
                let artifact = named_string(value, "name").map(|(found, _)| found);
                let inline = named_string(value, "version").map(|(found, range)| {
                    (
                        found,
                        start + value_start + range.start..start + value_start + range.end,
                    )
                });
                let reference = named_string(value, "version.ref")
                    .or_else(|| named_string(value, "ref"))
                    .map(|(found, _)| found);
                let (group, artifact) = match module {
                    Some((group, artifact)) => (Some(group), Some(artifact)),
                    None => (group, artifact),
                };
                libraries.push(CatalogLibrary {
                    group,
                    artifact,
                    reference,
                    inline,
                });
            }
            _ => {}
        }
    }
    let mut slots = Vec::new();
    for library in libraries {
        let (Some(group), Some(artifact)) = (library.group, library.artifact) else {
            continue;
        };
        let found = match (library.reference, library.inline) {
            (Some(alias), _) => versions.get(&alias).cloned(),
            (None, Some(inline)) => Some(inline),
            (None, None) => None,
        };
        if let Some((version, range)) = found {
            slots.push(Slot {
                group,
                artifact,
                version,
                range,
            });
        }
    }
    slots
}

/// The quoted text that starts at `start` of `line`.
fn quoted_at(line: &str, start: usize) -> Option<(String, Range<usize>)> {
    let quote = line[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let from = start + quote.len_utf8();
    let end = from + line[from..].find(quote)?;
    Some((line[from..end].to_owned(), from..end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POM: &str = r#"<project>
    <properties>
        <jackson.version>2.15.0</jackson.version>
        <unused.version>1.0</unused.version>
    </properties>
    <dependencies>
        <dependency>
            <groupId>com.fasterxml.jackson.core</groupId>
            <artifactId>jackson-databind</artifactId>
            <version>${jackson.version}</version>
        </dependency>
        <dependency>
            <groupId>org.example</groupId>
            <artifactId>plain</artifactId>
            <version>1.0</version>
        </dependency>
        <dependency>
            <groupId>org.example</groupId>
            <artifactId>managed</artifactId>
        </dependency>
    </dependencies>
    <build>
        <plugins>
            <plugin>
                <artifactId>spring-boot-maven-plugin</artifactId>
                <version>3.2.0</version>
            </plugin>
        </plugins>
    </build>
</project>
"#;

    #[test]
    fn versions_follow_the_order_of_maven() {
        let order = [
            ("1.0-alpha1", "1.0-beta1"),
            ("1.0-beta1", "1.0-milestone1"),
            ("1.0-milestone1", "1.0-rc1"),
            ("1.0-rc1", "1.0-snapshot"),
            ("1.0-snapshot", "1.0"),
            ("1.0", "1.0-sp"),
            ("1.0-sp", "1.0-zzz"),
            ("1.0", "1.0.1"),
            ("1.0.1", "1.0.2"),
            ("1.0", "1.1"),
            ("1.9", "1.10"),
            ("3.9.5", "3.10.0"),
            ("2.0", "2.1"),
            ("2.0.1", "2.0.2"),
            ("0.9", "1"),
            ("1.0", "1.0.0.1"),
            ("1.0-alpha-1", "1.0-alpha-2"),
            ("1.0-alpha-2", "1.0-alpha-10"),
            ("1.0-alpha", "1.0-beta"),
            ("1.0-beta", "1.0-milestone"),
            ("1.0-milestone", "1.0-rc"),
            ("1.0-rc", "1.0"),
            ("5.0-M1", "5.0-M2"),
            ("5.0-M2", "5.0-RC1"),
            ("5.0-RC1", "5.0"),
            ("1.0", "1.0.1-SNAPSHOT"),
            ("1.0.1-SNAPSHOT", "1.0.1"),
            ("1.0-alpha2snapshot", "1.0-alpha2"),
            ("1.0-1", "1.0-2"),
            ("1.0", "1.0-1"),
        ];
        for (lower, higher) in order {
            assert_eq!(
                compare(lower, higher),
                Ordering::Less,
                "{lower} is older than {higher}"
            );
            assert_eq!(
                compare(higher, lower),
                Ordering::Greater,
                "{higher} is newer than {lower}"
            );
        }
    }

    #[test]
    fn trailing_zeros_and_the_known_release_words_mean_the_same_version() {
        for (left, right) in [
            ("1.0.0", "1.0"),
            ("1", "1.0.0.0"),
            ("1.0-ga", "1.0"),
            ("1.0-final", "1.0.0"),
            ("1.0-release", "1.0"),
            ("1.0.0.RELEASE", "1.0"),
            ("5.0.0", "5.0"),
            ("1.0.0", "1"),
        ] {
            assert_eq!(compare(left, right), Ordering::Equal, "{left} == {right}");
        }
    }

    #[test]
    fn a_pre_release_is_recognized_in_every_spelling() {
        for version in [
            "1.0-alpha",
            "1.0-alpha1",
            "1.0-beta-2",
            "1.0-M1",
            "1.0-milestone-3",
            "1.0-rc1",
            "1.0-CR2",
            "2.0-SNAPSHOT",
            "3.0.0.a4",
        ] {
            assert!(is_prerelease(version), "{version}");
        }
        for version in ["1.0", "1.0.1", "1.0-sp1", "1.0.Final", "1.0.RELEASE"] {
            assert!(!is_prerelease(version), "{version}");
        }
    }

    #[test]
    fn a_stable_version_is_offered_over_a_pre_release() {
        let versions: Vec<String> = ["1.0", "1.1", "1.2-rc1", "2.0-M1", "1.1.5"]
            .iter()
            .map(|version| (*version).to_owned())
            .collect();

        let updates = updates_for("1.0", &versions);

        assert_eq!(updates.stable.as_deref(), Some("1.1.5"));
        assert_eq!(
            updates.patch, None,
            "the newest stable is the newest patch here"
        );
    }

    #[test]
    fn the_newest_stable_and_the_newest_patch_are_two_offers() {
        let versions: Vec<String> = ["1.1.0", "1.1.1", "1.1.2", "1.2.0", "1.3.0-rc1"]
            .iter()
            .map(|version| (*version).to_owned())
            .collect();

        let updates = updates_for("1.1.0", &versions);

        assert_eq!(updates.stable.as_deref(), Some("1.2.0"));
        assert_eq!(updates.patch.as_deref(), Some("1.1.2"));
    }

    #[test]
    fn nothing_is_offered_when_only_a_pre_release_is_newer() {
        let updates = updates_for("2.0", &["2.1-rc1".to_owned(), "2.0".to_owned()]);
        assert!(updates.is_empty());
        let updates = updates_for("2.1", &["2.1".to_owned(), "2.0".to_owned()]);
        assert!(updates.is_empty());
    }

    #[test]
    fn metadata_names_the_versions_of_an_artifact() {
        let xml = r#"<?xml version="1.0"?>
<metadata>
    <groupId>org.example</groupId>
    <artifactId>demo</artifactId>
    <versioning>
        <latest>2.0-sp1</latest>
        <release>2.0</release>
        <versions>
            <version>1.0</version>
            <version>1.1</version>
            <version>2.0</version>
        </versions>
        <lastUpdated>20250101000000</lastUpdated>
    </versioning>
</metadata>"#;

        let metadata = parse_metadata(xml);

        assert_eq!(metadata.latest.as_deref(), Some("2.0-sp1"));
        assert_eq!(metadata.release.as_deref(), Some("2.0"));
        assert_eq!(metadata.versions, ["1.0", "1.1", "2.0"]);
        assert_eq!(parse_metadata("<broken"), Metadata::default());
        assert_eq!(
            parse_metadata("<metadata/>"),
            Metadata::default(),
            "a file without versioning has no versions"
        );
    }

    #[test]
    fn a_pom_names_its_versions_directly_through_a_property_and_by_default_group() {
        let slots = sites(BuildFile::Pom, POM);
        let named = |artifact: &str| {
            slots
                .iter()
                .find(|slot| slot.artifact == artifact)
                .unwrap_or_else(|| panic!("a place for {artifact}"))
        };

        let property = named("jackson-databind");
        assert_eq!(property.group, "com.fasterxml.jackson.core");
        assert_eq!(property.version, "2.15.0");
        assert_eq!(
            &POM[property.range.clone()],
            "2.15.0",
            "the hint stands on the value of the property"
        );
        assert!(property.range.start < POM.find("<dependencies>").unwrap());

        let plain = named("plain");
        assert_eq!(plain.version, "1.0");
        assert_eq!(&POM[plain.range.clone()], "1.0");

        let plugin = named("spring-boot-maven-plugin");
        assert_eq!(plugin.group, "org.apache.maven.plugins");
        assert_eq!(plugin.version, "3.2.0");

        assert!(
            slots.iter().all(|slot| slot.artifact != "managed"),
            "a dependency without a version has no place"
        );
        assert!(
            slots
                .iter()
                .all(|slot| slot.version != "1.0" || slot.artifact != "unused.version"),
            "only a property a dependency uses is a place"
        );
    }

    #[test]
    fn a_property_of_two_artifacts_offers_nothing() {
        let text = r#"<project>
    <properties>
        <shared.version>1.0</shared.version>
    </properties>
    <dependencies>
        <dependency>
            <groupId>org.one</groupId>
            <artifactId>first</artifactId>
            <version>${shared.version}</version>
        </dependency>
        <dependency>
            <groupId>org.two</groupId>
            <artifactId>second</artifactId>
            <version>${shared.version}</version>
        </dependency>
    </dependencies>
</project>
"#;

        assert!(sites(BuildFile::Pom, text).is_empty());
        assert_eq!(
            slots(BuildFile::Pom, text).len(),
            2,
            "both are still places"
        );
    }

    #[test]
    fn a_gradle_script_names_versions_in_both_forms() {
        let text = r#"plugins {
    id 'java'
}
dependencies {
    implementation "org.example:demo:1.0"
    testImplementation 'org.example:test-lib:2.3.1'
    implementation group: 'com.example', name: 'mapped', version: '3.4.5'
    implementation("org.example:kotlin:4.0")
    implementation("g:a:$dynamic")
    implementation("g:a:1.+")
}
"#;

        let slots = sites(BuildFile::Script, text);
        let named = |artifact: &str| {
            slots
                .iter()
                .find(|slot| slot.artifact == artifact)
                .unwrap_or_else(|| panic!("a place for {artifact}"))
        };
        assert_eq!(named("demo").version, "1.0");
        assert_eq!(&text[named("demo").range.clone()], "1.0");
        assert_eq!(named("test-lib").group, "org.example");
        assert_eq!(named("mapped").version, "3.4.5");
        assert_eq!(named("kotlin").version, "4.0");
        assert!(
            slots.iter().all(|slot| slot.artifact != "a"),
            "a property and a dynamic version are no places"
        );
    }

    #[test]
    fn a_kotlin_script_names_the_map_entries_with_an_equal_sign() {
        let text = r#"dependencies {
    implementation(group = "org.example", name = "kotlin-mapped", version = "5.0")
}
"#;
        let slots = sites(BuildFile::Script, text);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].group, "org.example");
        assert_eq!(slots[0].artifact, "kotlin-mapped");
        assert_eq!(&text[slots[0].range.clone()], "5.0");
    }

    #[test]
    fn the_catalog_gives_a_version_through_a_reference_or_directly() {
        let text = r#"[versions]
jackson = "2.17.0"
inline = "6.0"

[libraries]
jackson-databind = { module = "com.fasterxml.jackson.core:jackson-databind", version.ref = "jackson" }
direct = { group = "org.example", name = "direct", version = "1.2.3" }
"#;

        let slots = sites(BuildFile::Catalog, text);
        let named = |artifact: &str| {
            slots
                .iter()
                .find(|slot| slot.artifact == artifact)
                .unwrap_or_else(|| panic!("a place for {artifact}"))
        };
        assert_eq!(named("jackson-databind").version, "2.17.0");
        assert_eq!(
            &text[named("jackson-databind").range.clone()],
            "2.17.0",
            "the reference points at the value in `versions`"
        );
        assert_eq!(named("direct").version, "1.2.3");
    }

    #[test]
    fn an_empty_version_is_a_place_for_completion() {
        let text = "<project><dependencies><dependency>\
            <groupId>org.example</groupId><artifactId>demo</artifactId>\
            <version></version></dependency></dependencies></project>";
        let slots = slots(BuildFile::Pom, text);
        assert_eq!(slots.len(), 1);
        assert!(slots[0].version.is_empty());
        assert_eq!(slots[0].range.start, text.find("</version>").unwrap());
        assert!(sites(BuildFile::Pom, text).is_empty());
        assert_eq!(
            slot_at(BuildFile::Pom, text, slots[0].range.start).map(|slot| slot.artifact),
            Some("demo".to_owned())
        );
    }

    #[test]
    fn the_cache_lives_a_day() {
        let root = test_support::TempDir::new("updates-cache");
        let file = cache_file(root.path(), "org.example", "demo").unwrap();
        assert!(file.ends_with("org/example/demo.json"));
        let versions = vec!["1.0".to_owned(), "1.1".to_owned()];

        write_cache(&file, &versions, 1_000).unwrap();

        assert_eq!(
            read_cache(&file, 1_000 + 60).as_deref(),
            Some(&versions[..])
        );
        assert_eq!(
            read_cache(&file, 1_000 + TTL.as_secs() + 1),
            None,
            "an old list is read from the repository again"
        );
        assert_eq!(read_cache(&root.path().join("missing.json"), 1_000), None);
    }

    #[test]
    fn the_memory_cache_expires_with_the_disk_one() {
        let root = test_support::TempDir::new("updates-memory");
        let source = VersionSource::new(root.path().to_path_buf());
        let versions = vec!["1.0".to_owned()];

        source.remember("org.example", "demo", &versions, 1_000, true);

        assert_eq!(
            source.cached("org.example", "demo", 1_100).as_deref(),
            Some(&versions[..])
        );
        let later = 1_000 + TTL.as_secs() + 1;
        assert_eq!(source.cached("org.example", "demo", later), None);
        assert_eq!(
            read_cache(
                &cache_file(root.path(), "org.example", "demo").unwrap(),
                later
            ),
            None
        );
    }

    #[test]
    fn a_name_that_is_no_path_builds_no_cache_file() {
        let root = Path::new("C:/cache");
        assert!(cache_file(root, "org..example", "demo").is_none());
        assert!(cache_file(root, "org/example", "demo").is_none());
        assert!(cache_file(root, "org.example", "../demo").is_none());
        assert!(cache_file(root, "org.example", "..").is_none());
        assert!(cache_file(root, "org.example", "").is_none());
        assert!(
            cache_file(root, "org.example", "demo.core").is_some(),
            "an artifact name may carry a dot"
        );
    }

    #[test]
    fn the_newest_version_comes_first_and_a_duplicate_comes_once() {
        let versions = ["1.9", "1.10", "1.10", "2.0-rc1"]
            .iter()
            .map(|version| (*version).to_owned())
            .collect::<Vec<_>>();

        assert_eq!(newest_first(&versions), ["2.0-rc1", "1.10", "1.9"]);
        assert_eq!(sorted(&versions), ["1.9", "1.10", "2.0-rc1"]);
    }

    #[test]
    fn the_repositories_of_a_project_end_with_central() {
        let root = test_support::TempDir::new("updates-repositories");
        let pom = "<project><repositories>\
            <repository><url>https://repo.example/maven/</url></repository>\
            <repository><url>https://user:secret@private.example/maven</url></repository>\
            <repository><url>${repo.url}</url></repository>\
            </repositories></project>";
        fs::write(root.path().join("pom.xml"), pom).unwrap();

        let urls = repositories(root.path());

        assert_eq!(
            urls,
            [
                "https://repo.example/maven".to_owned(),
                MAVEN_CENTRAL.to_owned()
            ],
            "a private repository and a property are left out"
        );
    }

    #[test]
    fn a_build_file_finds_its_own_kind() {
        assert_eq!(kind_of(Path::new("C:/p/pom.xml")), Some(BuildFile::Pom));
        assert_eq!(
            kind_of(Path::new("C:/p/build.gradle")),
            Some(BuildFile::Script)
        );
        assert_eq!(
            kind_of(Path::new("C:/p/build.gradle.kts")),
            Some(BuildFile::Script)
        );
        assert_eq!(
            kind_of(Path::new("C:/p/gradle/libs.versions.toml")),
            Some(BuildFile::Catalog)
        );
        assert_eq!(kind_of(Path::new("C:/p/settings.gradle")), None);
        assert_eq!(kind_of(Path::new("C:/p/other.xml")), None);
    }

    /// The network test is run by hand: `cargo test -p java-project -- --ignored`.
    #[test]
    #[ignore = "asks the real Maven Central"]
    fn the_real_repository_lists_spring_boot_versions() {
        let root = test_support::TempDir::new("updates-real");
        let cache = root.path().join("cache");
        let source = VersionSource::new(cache);
        let versions = source
            .versions(
                "org.springframework.boot",
                "spring-boot-starter-web",
                &[crate::sources::MAVEN_CENTRAL.to_owned()],
                now(),
            )
            .expect("the repository answers");
        assert!(
            versions.len() > 50,
            "the list holds the releases: {}",
            versions.len()
        );
        assert!(
            versions.iter().any(|version| version.starts_with("3.")),
            "the list holds the releases of the third line"
        );
        assert_eq!(
            source.versions(
                "org.springframework.boot",
                "spring-boot-starter-web",
                &[],
                now()
            ),
            Some(versions),
            "the second answer comes from the cache"
        );
    }
}
