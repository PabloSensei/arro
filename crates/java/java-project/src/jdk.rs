use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::Result;
use base::program::{executable, home_dir};

use crate::pom::read_pom;

/// Vendors install into these directories, each holds one JDK per subdirectory.
const WINDOWS_VENDORS: &[&str] = &[
    "Java",
    "Eclipse Adoptium",
    "Amazon Corretto",
    "Zulu",
    "Microsoft",
    "BellSoft",
    "Semeru",
];

/// The family of systems a list of JDK folders is made for. The search passes the one it
/// runs on and a test passes each of them, so every list is checked on any machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Windows,
    MacOs,
    Unix,
}

impl Family {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Unix
        }
    }
}

/// The folders of the machine a list of JDK install roots is built from. A test passes a
/// table of its own; the search fills it from the variables of the process.
#[derive(Clone, Debug, Default)]
pub struct Places {
    /// The home folder of the user.
    pub home: Option<PathBuf>,
    /// The folders where the vendors of Windows install: `ProgramW6432` and
    /// `ProgramFiles`, which a 32-bit process sees differently.
    pub program_files: Vec<PathBuf>,
    /// `LOCALAPPDATA`, where the per-user installs of Windows go.
    pub local_app_data: Option<PathBuf>,
}

impl Places {
    pub fn from_env() -> Self {
        let mut program_files: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles"]
            .iter()
            .filter_map(|variable| env::var_os(variable).map(PathBuf::from))
            .collect();
        // On a 64-bit process both variables name one folder.
        program_files.dedup();
        Self {
            home: home_dir(),
            program_files,
            local_app_data: env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

/// The folders whose subdirectories are JDK installs, in the order to search. The project
/// SDK and the JDK that runs the language server read the same list, so a JDK found by the
/// one is found by the other. Pure: `family` and `places` decide, and nothing is read.
pub fn roots_for(family: Family, places: &Places) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    match family {
        Family::Windows => {
            for base in &places.program_files {
                roots.extend(WINDOWS_VENDORS.iter().map(|vendor| base.join(vendor)));
            }
            if let Some(base) = &places.local_app_data {
                roots.push(base.join("Programs"));
            }
            if let Some(home) = &places.home {
                roots.push(home.join(".jdks"));
            }
        }
        Family::MacOs => {
            // A JDK of macOS is a bundle under one of these folders, as
            // `/usr/libexec/java_home` reports them.
            roots.push(
                Path::new("/Library")
                    .join("Java")
                    .join("JavaVirtualMachines"),
            );
            if let Some(home) = &places.home {
                roots.push(
                    home.join("Library")
                        .join("Java")
                        .join("JavaVirtualMachines"),
                );
                roots.push(home.join(".sdkman").join("candidates").join("java"));
                roots.push(home.join(".jdks"));
            }
            // Homebrew links every formula into `opt`; the JDKs there are named `openjdk`.
            roots.push(Path::new("/opt").join("homebrew").join("opt"));
            roots.push(Path::new("/usr").join("local").join("opt"));
        }
        Family::Unix => {
            roots.push(Path::new("/usr").join("lib").join("jvm"));
            if let Some(home) = &places.home {
                roots.push(home.join(".sdkman").join("candidates").join("java"));
                roots.push(home.join(".jdks"));
            }
            roots.push(
                Path::new("/home")
                    .join("linuxbrew")
                    .join(".linuxbrew")
                    .join("opt"),
            );
        }
    }
    roots
}

/// The home folder an entry of an install root names. A JDK of macOS is a bundle whose
/// `Contents/Home` is the home, and Homebrew keeps that bundle under `libexec`; the other
/// systems name the home itself. Pure apart from the look at the disk that tells a bundle
/// from a plain folder, so a test checks the bundles of macOS on any system.
pub fn home_in(family: Family, entry: PathBuf) -> PathBuf {
    if family != Family::MacOs {
        return entry;
    }
    let homebrew = entry
        .join("libexec")
        .join("openjdk.jdk")
        .join("Contents")
        .join("Home");
    if homebrew.is_dir() {
        return homebrew;
    }
    let bundle = entry.join("Contents").join("Home");
    if bundle.is_dir() {
        return bundle;
    }
    entry
}

/// The JDK homes under one install root, sorted by name: a root that is not there holds
/// none, and an entry that is not a folder is not a home.
pub fn homes_under(family: Family, root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut homes: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| home_in(family, path))
        .collect();
    homes.sort();
    homes
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jdk {
    pub home: PathBuf,
    /// The full version from the `release` file, like `1.8.0_492` or `25.0.3`.
    pub version: String,
    /// The number that matters: 8 for `1.8.0_492`, 25 for `25.0.3`.
    pub feature: u32,
}

impl Jdk {
    /// A JDK needs `java` as well as `javac`: a runtime without a compiler cannot build
    /// a project, and a folder left by a partial removal has neither the one nor the
    /// other, so such a record is never offered.
    pub fn at(home: &Path) -> Option<Self> {
        let bin = home.join("bin");
        if !bin.join(executable("javac")).is_file() || !bin.join(executable("java")).is_file() {
            return None;
        }
        let release = fs::read_to_string(home.join("release")).ok()?;
        let version = release.lines().find_map(|line| {
            let value = line.strip_prefix("JAVA_VERSION=")?;
            Some(value.trim().trim_matches('"').to_owned())
        })?;
        Some(Self {
            home: home.to_owned(),
            feature: feature_of(&version)?,
            version,
        })
    }

    pub fn java(&self) -> PathBuf {
        self.home.join("bin").join(executable("java"))
    }
}

/// Also takes what people write by hand: `17`, `1.8`, `temurin-21.0.2`, `25.0.3+9`.
pub fn feature_of(version: &str) -> Option<u32> {
    let version = version.rsplit('-').next()?;
    let mut parts = version.split(['.', '_', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    match first {
        1 => parts.next()?.parse().ok(),
        _ => Some(first),
    }
}

/// Every JDK on this machine, without duplicates: a link and its target count once.
pub fn find_jdks() -> Vec<Jdk> {
    let mut seen = HashSet::new();
    let found = candidates().into_iter().filter_map(|home| {
        let real = fs::canonicalize(&home).ok()?;
        seen.insert(real).then(|| Jdk::at(&home))?
    });
    found.collect()
}

fn candidates() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = env::var_os("JAVA_HOME")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    let compiler = executable("javac");
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            if directory.join(&compiler).is_file()
                && let Some(home) = directory.parent()
            {
                homes.push(home.to_owned());
            }
        }
    }
    let family = Family::current();
    for root in roots_for(family, &Places::from_env()) {
        homes.extend(homes_under(family, &root));
    }
    homes
}

/// What the project itself says about its JDK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requested {
    Feature(u32),
    Home(PathBuf),
    Nothing,
}

/// Looks through the places in the order from the architecture document. Reads files only.
pub fn requested_jdk(root: &Path) -> Requested {
    let feature = version_file(root)
        .or_else(|| pom_release(root))
        .or_else(|| toolchain(root));
    if let Some(feature) = feature {
        return Requested::Feature(feature);
    }
    if let Some(home) = property(&root.join("gradle.properties"), "org.gradle.java.home") {
        return Requested::Home(home.into());
    }
    let criteria = root.join("gradle/gradle-daemon-jvm.properties");
    let feature = property(&criteria, "toolchainVersion").and_then(|value| feature_of(&value));
    feature.map_or(Requested::Nothing, Requested::Feature)
}

fn version_file(root: &Path) -> Option<u32> {
    let text = fs::read_to_string(root.join(".java-version")).ok()?;
    feature_of(text.lines().next()?.trim())
}

fn pom_release(root: &Path) -> Option<u32> {
    feature_of(&read_pom(&root.join("pom.xml")).ok()?.java_release?)
}

/// A build script is a program, so this finds only the usual spellings.
fn toolchain(root: &Path) -> Option<u32> {
    ["build.gradle", "build.gradle.kts"]
        .iter()
        .find_map(|name| {
            let script = fs::read_to_string(root.join(name)).ok()?;
            number_after(&script, "JavaLanguageVersion.of(")
                .or_else(|| number_after(&script, "JavaVersion.VERSION_"))
        })
}

fn number_after(text: &str, marker: &str) -> Option<u32> {
    let (_, rest) = text.split_once(marker)?;
    let rest = rest.strip_prefix("1_").unwrap_or(rest);
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The value of one key in a `.properties` file, with `\:` and `\\` undone.
fn property(file: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        let value = value.trim().replace("\\:", ":").replace("\\\\", "\\");
        (name.trim() == key).then_some(value)
    })
}

/// The same version when there is one, then the nearest newer: a newer JDK builds an older
/// release, the other way around does not work. Without a request, the JDK the user set
/// up as the default, then the newest.
pub fn choose_jdk(requested: &Requested, jdks: &[Jdk], default_home: Option<&Path>) -> Result<Jdk> {
    let found = match requested {
        Requested::Home(home) => {
            let jdk = jdks.iter().find(|jdk| jdk.home == *home).cloned();
            let reason = || format!("no JDK at {}", home.display());
            return jdk.or_else(|| Jdk::at(home)).ok_or_else(|| reason().into());
        }
        Requested::Feature(feature) => jdks
            .iter()
            .filter(|jdk| jdk.feature >= *feature)
            .min_by_key(|jdk| jdk.feature),
        Requested::Nothing => {
            let default = jdks
                .iter()
                .find(|jdk| Some(jdk.home.as_path()) == default_home);
            default.or_else(|| jdks.iter().max_by_key(|jdk| jdk.feature))
        }
    };
    let reason = || match requested {
        Requested::Feature(feature) => format!("the project needs JDK {feature}, none found"),
        _ => "no JDK found".to_owned(),
    };
    found.cloned().ok_or_else(|| reason().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk(feature: u32) -> Jdk {
        Jdk {
            home: format!("jdk-{feature}").into(),
            version: feature.to_string(),
            feature,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        test_support::scratch(&format!("project-jdk-{name}"))
    }

    #[test]
    fn versions_people_write_by_hand_are_understood() {
        let cases = [
            ("1.8.0_492", 8),
            ("1.8", 8),
            ("8", 8),
            ("17", 17),
            ("21.0.2", 21),
            ("25.0.3+9", 25),
            ("temurin-21.0.2", 21),
        ];
        for (text, feature) in cases {
            assert_eq!(feature_of(text), Some(feature), "{text}");
        }
        assert_eq!(feature_of("system"), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_release_file_gives_the_version() {
        let home = temp_dir("home");
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/javac.exe"), "").unwrap();
        fs::write(home.join("bin/java.exe"), "").unwrap();
        fs::write(
            home.join("release"),
            "IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"1.8.0_492\"\n",
        )
        .unwrap();

        let found = Jdk::at(&home).unwrap();

        assert_eq!((found.version.as_str(), found.feature), ("1.8.0_492", 8));
    }

    #[test]
    fn a_runtime_without_a_compiler_is_not_a_jdk() {
        let home = temp_dir("runtime");
        fs::write(home.join("release"), "JAVA_VERSION=\"17.0.1\"\n").unwrap();

        assert_eq!(Jdk::at(&home), None);
    }

    #[test]
    fn the_nearest_newer_jdk_stands_in_for_a_missing_one() {
        let jdks = [jdk(8), jdk(25), jdk(17)];

        let chosen = choose_jdk(&Requested::Feature(11), &jdks, None).unwrap();

        assert_eq!(chosen.feature, 17);
    }

    #[test]
    fn a_request_that_nothing_satisfies_says_what_is_missing() {
        let error = choose_jdk(&Requested::Feature(21), &[jdk(17)], None).unwrap_err();

        assert!(error.to_string().contains("JDK 21"), "{error}");
    }

    #[test]
    fn without_a_request_the_default_comes_first_then_the_newest() {
        let jdks = [jdk(8), jdk(25), jdk(17)];
        let default = Path::new("jdk-17");

        let chosen = choose_jdk(&Requested::Nothing, &jdks, Some(default)).unwrap();
        assert_eq!(chosen.feature, 17);

        let chosen = choose_jdk(&Requested::Nothing, &jdks, None).unwrap();
        assert_eq!(chosen.feature, 25);
    }

    #[test]
    fn the_places_are_read_in_order() {
        let root = temp_dir("order");
        assert_eq!(requested_jdk(&root), Requested::Nothing);

        fs::create_dir_all(root.join("gradle")).unwrap();
        let criteria = "toolchainVersion=21\n";
        fs::write(root.join("gradle/gradle-daemon-jvm.properties"), criteria).unwrap();
        assert_eq!(requested_jdk(&root), Requested::Feature(21));

        let properties = "org.gradle.java.home=C\\:\\\\jdk\n";
        fs::write(root.join("gradle.properties"), properties).unwrap();
        assert_eq!(requested_jdk(&root), Requested::Home("C:\\jdk".into()));

        let script = "java { toolchain { languageVersion = JavaLanguageVersion.of(11) } }";
        fs::write(root.join("build.gradle"), script).unwrap();
        assert_eq!(requested_jdk(&root), Requested::Feature(11));

        let pom = "<project><properties><maven.compiler.release>9</maven.compiler.release>\
                   </properties></project>";
        fs::write(root.join("pom.xml"), pom).unwrap();
        assert_eq!(requested_jdk(&root), Requested::Feature(9));

        fs::write(root.join(".java-version"), "temurin-8.0.1\n").unwrap();
        assert_eq!(requested_jdk(&root), Requested::Feature(8));
    }

    #[test]
    fn old_and_new_spellings_of_a_language_level_are_read() {
        assert_eq!(
            number_after("JavaVersion.VERSION_17;", "JavaVersion.VERSION_"),
            Some(17)
        );
        assert_eq!(
            number_after("JavaVersion.VERSION_1_8;", "JavaVersion.VERSION_"),
            Some(8)
        );
    }

    #[test]
    fn every_system_has_its_own_install_roots() {
        let places = Places {
            home: Some(PathBuf::from("/home/user")),
            program_files: Vec::new(),
            local_app_data: None,
        };

        let mac = roots_for(Family::MacOs, &places);
        assert!(
            mac.iter()
                .any(|root| root.as_path() == Path::new("/Library/Java/JavaVirtualMachines")),
            "{mac:?}"
        );
        assert!(
            mac.iter()
                .any(|root| root.as_path()
                    == Path::new("/home/user/Library/Java/JavaVirtualMachines"))
        );
        assert!(
            mac.iter()
                .any(|root| root.as_path() == Path::new("/opt/homebrew/opt")),
            "{mac:?}"
        );
        assert!(
            mac.iter()
                .any(|root| root.as_path() == Path::new("/usr/local/opt"))
        );

        let linux = roots_for(Family::Unix, &places);
        assert!(
            linux
                .iter()
                .any(|root| root.as_path() == Path::new("/usr/lib/jvm"))
        );
        assert!(
            linux
                .iter()
                .any(|root| root.as_path() == Path::new("/home/user/.sdkman/candidates/java"))
        );
        assert!(
            !linux
                .iter()
                .any(|root| root.as_path() == Path::new("/Library/Java/JavaVirtualMachines"))
        );

        let windows = roots_for(
            Family::Windows,
            &Places {
                program_files: vec![PathBuf::from(r"C:\Program Files")],
                local_app_data: Some(PathBuf::from(r"C:\Users\user\AppData\Local")),
                ..places.clone()
            },
        );
        assert!(
            windows
                .iter()
                .any(|root| root.as_path() == Path::new(r"C:\Program Files\Java"))
        );
        assert!(
            windows
                .iter()
                .any(|root| root.as_path() == Path::new(r"C:\Users\user\AppData\Local\Programs"))
        );
    }

    #[test]
    fn a_mac_jdk_is_a_bundle_that_homebrew_keeps_under_libexec() {
        let bundle = temp_dir("bundle");
        std::fs::create_dir_all(bundle.join("Contents").join("Home")).unwrap();

        assert_eq!(
            home_in(Family::MacOs, bundle.clone()),
            bundle.join("Contents").join("Home")
        );
        assert_eq!(home_in(Family::Unix, bundle.clone()), bundle);

        let brew = temp_dir("brew");
        let nested = brew
            .join("libexec")
            .join("openjdk.jdk")
            .join("Contents")
            .join("Home");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(home_in(Family::MacOs, brew), nested);
    }
}
