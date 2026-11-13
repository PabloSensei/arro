//! A harness found on a Unix PATH: the entry point of the package manager itself, with
//! no batch file in front of it.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use agent_core::{Harness, detect};

struct Directory(test_support::TempDir);

impl Directory {
    fn new(name: &str) -> Self {
        Self(test_support::TempDir::new(&format!("harness-unix-{name}")))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn touch(&self, name: &str) -> PathBuf {
        let file = self.path().join(name);
        fs::write(&file, "#!/bin/sh\n").expect("file");
        file
    }
}

#[test]
fn an_entry_point_of_the_package_manager_is_found_as_it_is() {
    let directory = Directory::new("plain");
    let program = directory.touch("claude");

    let found = detect(directory.path().as_os_str());

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].harness, Harness::Claude);
    assert_eq!(found[0].program, program);
    assert!(!found[0].through_shell);
}

#[test]
fn a_batch_file_of_another_system_is_not_a_harness() {
    let directory = Directory::new("batch");
    directory.touch("opencode.cmd");

    assert!(detect(directory.path().as_os_str()).is_empty());
}
