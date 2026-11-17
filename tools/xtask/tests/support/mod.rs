// Every test binary compiles this module and uses only a part of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// A throwaway Cargo workspace outside the repository, so it never joins the real one.
pub struct Fixture {
    pub root: PathBuf,
    members: Vec<String>,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        Self {
            root,
            members: Vec::new(),
        }
    }

    /// Adds a crate at `path`. `dependencies` are other crates of the fixture.
    pub fn krate(self, path: &str, dependencies: &[&str]) -> Self {
        self.krate_with(path, "", dependencies)
    }

    /// Same as `krate`, with extra lines for the `[package]` table.
    pub fn krate_with(mut self, path: &str, package_lines: &str, dependencies: &[&str]) -> Self {
        let name = path.rsplit('/').next().unwrap();
        let mut manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             {package_lines}\n\n[dependencies]\n"
        );
        let to_root = "../".repeat(path.split('/').count());
        for dependency in dependencies {
            let target = dependency.rsplit('/').next().unwrap();
            manifest.push_str(&format!(
                "{target} = {{ path = \"{to_root}{dependency}\" }}\n"
            ));
        }
        self.write(&format!("{path}/Cargo.toml"), &manifest);
        self.write(&format!("{path}/src/lib.rs"), "");

        self.members.push(format!("\"{path}\""));
        let members = self.members.join(", ");
        self.write(
            "Cargo.toml",
            &format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
        );
        self
    }

    pub fn source(self, path: &str, text: &str) -> Self {
        self.write(path, text);
        self
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}
