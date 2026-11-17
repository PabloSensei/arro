//! The last client of an adapter closes its input, and the adapter process that reads
//! that input leaves. The stand-in adapter is a small script of this test; no debugger
//! is needed.

use std::path::Path;
use std::time::{Duration, Instant};

use dap_client::{AdapterProcess, Client, Event};

/// How long the stand-in adapter may take to see the end of its input and leave.
const LIMIT: Duration = Duration::from_secs(15);

fn script(folder: &Path, marker: &Path) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let file = folder.join("adapter.bat");
        let text = format!(
            "@echo off\r\nmore > nul\r\necho done> \"{}\"\r\n",
            marker.display()
        );
        std::fs::write(&file, text).unwrap();
        (
            "cmd".to_owned(),
            vec!["/d".into(), "/c".into(), file.display().to_string()],
        )
    }
    #[cfg(unix)]
    {
        let file = folder.join("adapter.sh");
        let text = format!("#!/bin/sh\ncat > /dev/null\n: > \"{}\"\n", marker.display());
        std::fs::write(&file, text).unwrap();
        ("sh".to_owned(), vec![file.display().to_string()])
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_last_client_closes_the_adapter_input_and_the_process_leaves() {
    let folder = test_support::TempDir::new("dap-adapter-close");
    let marker = folder.join("done.txt");
    let (program, arguments) = script(folder.path(), &marker);
    let mut child = tokio::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the stand-in adapter starts");
    let input = child.stdin.take().expect("the adapter input");
    let output = child.stdout.take().expect("the adapter output");
    let mut process = child;

    // The stand-in adapter writes the marker once its input ends, then exits on its own.
    let adapter = AdapterProcess::from_shutdown(move |grace| {
        let started = Instant::now();
        while started.elapsed() < grace {
            match process.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = process.start_kill();
        let _ = process.try_wait();
    });
    let (client, mut events) = Client::start_with_adapter(output, input, adapter);
    let second = client.clone();
    drop(client);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "the adapter left while a client was still open"
    );

    drop(second);
    let started = Instant::now();
    while !marker.exists() {
        assert!(
            started.elapsed() < LIMIT,
            "the adapter did not see the end of its input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ended = tokio::time::timeout(LIMIT, events.recv())
        .await
        .expect("the reader did not see the end of the output");
    assert!(
        matches!(ended, Some(Event::Closed) | None),
        "the reader ended with {ended:?}"
    );
}
