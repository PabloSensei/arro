//! T-0288: the journal of the stand-in `kubectl`. The Kubernetes node asks the watch, an
//! action and a live log at the same time, so several copies of the script run together;
//! no call may go missing because another one writes its journal at the same moment.
//!
//! cargo test -p panels window_tests_kubernetes_journal

#![cfg(windows)]

use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::window_tests_kubernetes::{fake_kubectl, journal_of};

/// Starts the stand-in with the arguments of one call.
fn call(script: &Path, args: &[&str]) -> Child {
    Command::new(script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in kubectl starts")
}

#[test]
fn calls_that_write_the_journal_together_all_reach_it() {
    let (script, journal) = fake_kubectl("kube-journal-together");
    let writers = 12;
    let mut children = Vec::new();
    for index in 0..writers {
        let name = format!("web-{index}");
        children.push(call(&script, &["delete", "pods", &name]));
    }
    for mut child in children {
        assert!(
            child.wait().expect("the stand-in kubectl ends").success(),
            "the stand-in kubectl answers"
        );
    }

    let journal = journal_of(&journal);
    for index in 0..writers {
        let line = format!("delete pods web-{index}");
        assert!(
            journal.lines().any(|row| row == line),
            "`{line}` is in the journal: {journal:?}"
        );
    }
}
