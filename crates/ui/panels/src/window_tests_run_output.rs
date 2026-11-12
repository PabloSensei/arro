//! Output of several processes that write into one console of the Run window: a piece
//! of a line stays with the process and the stream that wrote it, and a line a process
//! left open comes when that process is gone.

use crate::ansi::OutputBuffers;

fn texts(done: &[crate::ansi::Line]) -> Vec<&str> {
    done.iter().map(|line| line.text.as_str()).collect()
}

#[test]
fn unfinished_lines_of_two_processes_do_not_join() {
    let mut output = OutputBuffers::default();
    let mut done = Vec::new();

    output.push(1, false, "abc", &mut done);
    output.push(2, false, "xyz", &mut done);
    assert!(done.is_empty(), "a line is shown only when it ends");

    output.push(1, false, "\n", &mut done);
    output.push(2, false, "\n", &mut done);

    assert_eq!(texts(&done), ["abc", "xyz"]);
}

#[test]
fn stdout_and_stderr_of_one_process_do_not_join() {
    let mut output = OutputBuffers::default();
    let mut done = Vec::new();

    output.push(1, false, "out", &mut done);
    output.push(1, true, "err", &mut done);
    output.push(1, false, "\n", &mut done);
    output.push(1, true, "\n", &mut done);

    assert_eq!(texts(&done), ["out", "err"]);
    assert!(!done[0].spans[0].1.error, "the line of stdout is no error");
    assert!(done[1].spans[0].1.error, "the line of stderr is an error");
}

#[test]
fn a_line_without_an_end_comes_when_its_process_closes() {
    let mut output = OutputBuffers::default();
    let mut done = Vec::new();

    output.push(7, false, "tail", &mut done);
    output.push(8, false, "held", &mut done);
    assert!(done.is_empty(), "no line ends yet");

    output.finish(7, &mut done);
    assert_eq!(texts(&done), ["tail"]);
    assert_eq!(
        output.current().map(|line| line.text.as_str()),
        Some("held"),
        "the line of the other process stays unfinished"
    );

    output.finish_all(&mut done);
    assert_eq!(texts(&done), ["tail", "held"]);
}
