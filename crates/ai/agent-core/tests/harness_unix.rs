//! A harness found on a Unix PATH: the entry point of the package manager itself, with
//! no batch file in front of it.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

use agent_core::{Harness, detect};

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("harness-unix-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("directory");
        Self(path)
    }

    fn touch(&self, name: &str) -> PathBuf {
        let file = self.0.join(name);
        fs::write(&file, "#!/bin/sh\n").expect("file");
        file
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_entry_point_of_the_package_manager_is_found_as_it_is() {
    let directory = Directory::new("plain");
    let program = directory.touch("claude");

    let found = detect(directory.0.as_os_str());

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].harness, Harness::Claude);
    assert_eq!(found[0].program, program);
    assert!(!found[0].through_shell);
}

#[test]
fn a_batch_file_of_another_system_is_not_a_harness() {
    let directory = Directory::new("batch");
    directory.touch("opencode.cmd");

    assert!(detect(directory.0.as_os_str()).is_empty());
}
