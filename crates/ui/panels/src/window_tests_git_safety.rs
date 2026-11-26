//! Git commands that touch one file leave the editors of the other files alone: a Rollback of
//! one file keeps the unsaved text of an open editor of another file, and the editor of the
//! rolled back file shows the text that is on the disk again.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{buffer_of, draw, editor_of, git_repository, open_path, open_window};
use crate::window_tests_git::{git, git_state, wait_for};

#[ui_kit::gpui::test]
fn safety_rollback_of_one_file_keeps_the_unsaved_text_of_another(cx: &mut TestAppContext) {
    let root = git_repository("safety-rollback");
    let runner = git();
    fs::write(root.join("other.txt"), "one\n").unwrap();
    runner.run(&root, ["add", "other.txt"]).unwrap();
    runner.run(&root, ["commit", "-m", "other"]).unwrap();
    fs::write(root.join("other.txt"), "changed\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the repository", |cx| {
        git_state(cx).read_with(cx, |git, _| git.repo().is_some())
    });

    let other = root.join("other.txt");
    open_path(cx, window, &other);
    let other_view = editor_of(cx, window, &other);
    assert_eq!(buffer_of(cx, &other_view), "changed\n");
    // Leaving a tab writes its unsaved text, so the edit is the last thing before the rollback.
    let main = root.join("main.txt");
    open_path(cx, window, &main);
    let main_view = editor_of(cx, window, &main);
    main_view.update(cx, |view, cx| view.replace_text(0..5, "UNSAVED", cx));

    git_state(cx).update(cx, |git, cx| {
        git.revert_paths(vec!["other.txt".to_owned()], cx)
    });

    wait_for(cx, "the rolled back editor", |cx| {
        buffer_of(cx, &other_view) == "one\n"
    });
    assert_eq!(
        buffer_of(cx, &main_view),
        "UNSAVED\nbeta\n",
        "the unsaved text of the other file stays"
    );
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        "alpha\nbeta\n",
        "nothing saved the other file"
    );
    // Git writes the line ends of the machine into the file it checks out.
    assert_eq!(
        fs::read_to_string(&other).unwrap().replace("\r\n", "\n"),
        "one\n",
        "the rollback took the change back"
    );
}
