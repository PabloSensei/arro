//! The Spring Initializr service: the metadata of a server, the cache of it for a day,
//! and the address of the archive of a new project. Every call blocks on the network,
//! so a caller asks from a thread of its own and never from the window.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use project_model::scaffold::Choice;
use serde::{Deserialize, Serialize};

/// The server of the Spring team, the one IDEA offers first.
pub const DEFAULT_SERVER: &str = "https://start.spring.io";

/// How long the metadata of a server stays good.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The media type of the metadata of a v2.2 server.
pub const METADATA_TYPE: &str = "application/vnd.initializr.v2.2+json";

/// How long a server has to answer.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The largest answer the service may give.
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// The keys of [`Metadata::choices`] and [`Metadata::default_of`], as the fields of the
/// form name them.
pub const BOOT_VERSION: &str = "boot";
pub const JAVA_VERSION: &str = "java";
pub const PROJECT_TYPE: &str = "type";
pub const PACKAGING: &str = "packaging";
pub const LANGUAGE: &str = "language";

/// What a server offers: the choices of the form and the dependencies with their groups.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    /// The value a server offers first, by field key.
    pub defaults: BTreeMap<String, String>,
    /// The values of every field, by field key.
    pub choices: BTreeMap<String, Vec<Choice>>,
    /// Every dependency, with the group in [`Choice::group`].
    pub dependencies: Vec<Choice>,
}

impl Metadata {
    pub fn choices(&self, key: &str) -> &[Choice] {
        self.choices.get(key).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn default_of(&self, key: &str) -> Option<&str> {
        self.defaults.get(key).map(String::as_str)
    }

    /// The dependencies the server offers, which the ignored network test reports.
    pub fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }
}

/// The metadata of a server, from the cache while it is younger than [`TTL`], else from
/// the server itself.
pub fn fetch(server: &str, cache: &Path) -> Result<Metadata, String> {
    fetch_at(server, cache, now())
}

/// [`fetch`] with the moment given, so a test needs no clock.
pub fn fetch_at(server: &str, cache: &Path, now: u64) -> Result<Metadata, String> {
    if let Some(metadata) = read_cache(cache, server, now) {
        return Ok(metadata);
    }
    let name = server_name(server);
    let url = format!("{}/", server.trim_end_matches('/'));
    let body = get(&url, METADATA_TYPE, &name)?;
    let text = String::from_utf8_lossy(&body).into_owned();
    let metadata =
        parse(&text).map_err(|error| format!("The metadata of {name} cannot be read: {error}"))?;
    write_cache(cache, server, &text, now);
    Ok(metadata)
}

/// The host of a server, for the messages the user reads.
pub fn server_name(server: &str) -> String {
    let without_scheme = server
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(server);
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    if host.is_empty() {
        server.trim().to_owned()
    } else {
        host.to_owned()
    }
}

/// Reads the answer of a server. Unknown parts are left out, so a newer server still
/// gives what the form can show.
pub fn parse(text: &str) -> Result<Metadata, String> {
    let raw: RawMetadata = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let mut metadata = Metadata::default();
    for (key, choices) in [
        (BOOT_VERSION, raw.boot_version),
        (JAVA_VERSION, raw.java_version),
        (PROJECT_TYPE, raw.kind),
        (PACKAGING, raw.packaging),
        (LANGUAGE, raw.language),
    ] {
        let Some(choices) = choices else {
            continue;
        };
        let values: Vec<Choice> = choices
            .values
            .into_iter()
            .map(|value| Choice::new(value.id, value.name))
            .collect();
        if let Some(default) = choices
            .default
            .filter(|default| values.iter().any(|choice| choice.value == *default))
        {
            metadata.defaults.insert(key.to_owned(), default);
        } else if let Some(first) = values.first() {
            metadata
                .defaults
                .insert(key.to_owned(), first.value.clone());
        }
        metadata.choices.insert(key.to_owned(), values);
    }
    let groups = raw
        .dependencies
        .map(|dependencies| dependencies.values)
        .unwrap_or_default();
    for group in groups {
        for dependency in group.values {
            let mut choice = Choice::new(dependency.id, dependency.name);
            if !group.name.is_empty() {
                choice = choice.in_group(group.name.clone());
            }
            if let Some(description) = dependency.description {
                choice = choice.with_hint(description);
            }
            metadata.dependencies.push(choice);
        }
    }
    Ok(metadata)
}

/// The request of a starter archive: the fields of the form as the service names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StarterRequest {
    pub server: String,
    /// The project type of the service, such as `maven-project`.
    pub kind: String,
    pub language: String,
    pub boot: String,
    pub group: String,
    pub artifact: String,
    pub name: String,
    pub description: String,
    pub package: String,
    pub packaging: String,
    pub java: String,
    pub dependencies: Vec<String>,
}

impl StarterRequest {
    /// `GET /starter.zip` with every field of the form as a query parameter.
    pub fn url(&self) -> String {
        let mut url = format!("{}/starter.zip?", self.server.trim_end_matches('/'));
        let mut first = true;
        for (key, value) in self.parameters() {
            if !first {
                url.push('&');
            }
            first = false;
            url.push_str(key);
            url.push('=');
            url.push_str(&encode(&value));
        }
        url
    }

    /// The parameters in the order the service documents them, the dependencies last
    /// and one parameter each.
    fn parameters(&self) -> Vec<(&'static str, String)> {
        let mut parameters = vec![
            ("type", self.kind.clone()),
            ("language", self.language.clone()),
            ("bootVersion", self.boot.clone()),
            ("baseDir", self.name.clone()),
            ("groupId", self.group.clone()),
            ("artifactId", self.artifact.clone()),
            ("name", self.name.clone()),
            ("description", self.description.clone()),
            ("packageName", self.package.clone()),
            ("packaging", self.packaging.clone()),
            ("javaVersion", self.java.clone()),
        ];
        parameters.extend(
            self.dependencies
                .iter()
                .map(|dependency| ("dependencies", dependency.clone())),
        );
        parameters
    }

    /// Downloads the archive of the project. Blocks.
    pub fn download(&self) -> Result<Vec<u8>, String> {
        let name = server_name(&self.server);
        get(&self.url(), "application/zip", &name)
    }
}

/// One get request of the service, with the media type it asks for and a message the
/// user understands when the server cannot answer.
fn get(url: &str, accept: &str, name: &str) -> Result<Vec<u8>, String> {
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
    let response = agent
        .get(url)
        .header("Accept", accept)
        .call()
        .map_err(|_| format!("Cannot reach {name}"))?;
    if !response.status().is_success() {
        return Err(format!("{name} answered {}", response.status()));
    }
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|_| format!("Cannot reach {name}"))?;
    Ok(body)
}

/// Keeps the characters a query allows and escapes the rest, so a dependency of spaces
/// or a group with dots reaches the service as it was written.
fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Where the metadata of one server waits in the cache of the IDE.
fn cache_file(cache: &Path, server: &str) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in server.trim().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    cache.join(format!("initializr-{hash:016x}.json"))
}

/// The cached answer while it is younger than [`TTL`].
fn read_cache(cache: &Path, server: &str, now: u64) -> Option<Metadata> {
    let text = fs::read_to_string(cache_file(cache, server)).ok()?;
    let cached: Cached = serde_json::from_str(&text).ok()?;
    if now.saturating_sub(cached.fetched) >= TTL.as_secs() {
        return None;
    }
    parse(&cached.body).ok()
}

/// A cache that cannot be written leaves the answer as it is: the form still works, the
/// next open asks the server again.
fn write_cache(cache: &Path, server: &str, body: &str, now: u64) {
    let file = cache_file(cache, server);
    let Some(directory) = file.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let cached = Cached {
        fetched: now,
        body: body.to_owned(),
    };
    if let Ok(text) = serde_json::to_string(&cached) {
        let _ = fs::write(file, text);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct RawMetadata {
    #[serde(rename = "bootVersion")]
    boot_version: Option<RawChoices>,
    #[serde(rename = "javaVersion")]
    java_version: Option<RawChoices>,
    #[serde(rename = "type")]
    kind: Option<RawChoices>,
    packaging: Option<RawChoices>,
    language: Option<RawChoices>,
    dependencies: Option<RawDependencies>,
}

#[derive(Deserialize)]
struct RawChoices {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    values: Vec<RawChoice>,
}

#[derive(Deserialize)]
struct RawChoice {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawDependencies {
    #[serde(default)]
    values: Vec<RawGroup>,
}

#[derive(Deserialize)]
struct RawGroup {
    #[serde(default)]
    name: String,
    #[serde(default)]
    values: Vec<RawChoice>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    fetched: u64,
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        test_support::scratch(&format!("initializr-{name}"))
    }

    fn sample() -> &'static str {
        include_str!("../tests/data/initializr/metadata.json")
    }

    #[test]
    fn an_answer_gives_the_versions_and_the_grouped_dependencies() {
        let metadata = parse(sample()).unwrap();

        assert_eq!(metadata.default_of(BOOT_VERSION), Some("4.0.0"));
        assert_eq!(
            metadata
                .choices(JAVA_VERSION)
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            ["17", "21", "25"]
        );
        assert_eq!(
            metadata
                .choices(PROJECT_TYPE)
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["Maven", "Gradle - Groovy", "Gradle - Kotlin"]
        );
        assert_eq!(metadata.default_of(PACKAGING), Some("jar"));
        assert_eq!(metadata.dependency_count(), 3);
        let web = &metadata.dependencies[0];
        assert_eq!(
            (web.value.as_str(), web.label.as_str()),
            ("web", "Spring Web")
        );
        assert_eq!(web.group.as_deref(), Some("Web"));
        assert!(
            metadata.dependencies[0..2]
                .iter()
                .all(|choice| choice.group.as_deref() == Some("Web"))
        );
        assert_eq!(
            metadata.dependencies[2].group.as_deref(),
            Some("Developer Tools")
        );
    }

    #[test]
    fn a_part_of_the_answer_the_form_cannot_show_is_left_out() {
        let metadata = parse(r#"{"bootVersion": {"default": "9", "values": []}}"#).unwrap();

        assert!(metadata.choices(BOOT_VERSION).is_empty());
        assert_eq!(metadata.default_of(BOOT_VERSION), None);
        assert_eq!(metadata.dependency_count(), 0);
    }

    #[test]
    fn the_address_of_a_starter_names_every_field_and_the_dependencies() {
        let request = StarterRequest {
            server: "https://start.spring.io/".to_owned(),
            kind: "maven-project".to_owned(),
            language: "java".to_owned(),
            boot: "4.0.0".to_owned(),
            group: "org.example".to_owned(),
            artifact: "shop-front".to_owned(),
            name: "Shop Front".to_owned(),
            description: "Demo project for Spring Boot".to_owned(),
            package: "org.example".to_owned(),
            packaging: "jar".to_owned(),
            java: "21".to_owned(),
            dependencies: vec!["web".to_owned(), "actuator".to_owned()],
        };

        let url = request.url();

        assert_eq!(
            url,
            "https://start.spring.io/starter.zip?type=maven-project&language=java&bootVersion=4.0.0&baseDir=Shop%20Front&groupId=org.example&artifactId=shop-front&name=Shop%20Front&description=Demo%20project%20for%20Spring%20Boot&packageName=org.example&packaging=jar&javaVersion=21&dependencies=web&dependencies=actuator"
        );
    }

    #[test]
    fn the_metadata_lives_a_day() {
        let cache = scratch("cache");
        let server = "http://127.0.0.1:1";
        write_cache(&cache, server, sample(), 1_000);

        let metadata = fetch_at(server, &cache, 1_000 + 60).unwrap();
        assert_eq!(metadata.dependency_count(), 3);

        let expired = fetch_at(server, &cache, 1_000 + TTL.as_secs() + 1);
        assert_eq!(
            expired.unwrap_err(),
            "Cannot reach 127.0.0.1:1",
            "the cache is over and the server cannot answer"
        );
    }

    #[test]
    fn a_server_that_does_not_answer_says_so() {
        let cache = scratch("offline");
        let error = fetch_at("http://127.0.0.1:1", &cache, 1_000).unwrap_err();

        assert_eq!(error, "Cannot reach 127.0.0.1:1");
        assert_eq!(server_name("https://start.spring.io"), "start.spring.io");
        assert_eq!(server_name("http://127.0.0.1:8080/"), "127.0.0.1:8080");
    }

    /// The real service: the network test the task asks to run once, by hand.
    #[test]
    #[ignore = "network: reads the metadata of the real Spring Initializr"]
    fn the_real_service_answers_with_its_dependencies() {
        let cache = scratch("network");
        let metadata = fetch(DEFAULT_SERVER, &cache).unwrap();
        println!(
            "start.spring.io offers {} dependencies, {} Spring Boot versions",
            metadata.dependency_count(),
            metadata.choices(BOOT_VERSION).len()
        );
        assert!(metadata.dependency_count() > 50, "the metadata is empty");
        assert!(!metadata.choices(BOOT_VERSION).is_empty());
    }
}
