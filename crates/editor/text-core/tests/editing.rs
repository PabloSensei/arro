//! The everyday editing commands on a `Buffer`. Texts mark carets with `|` and selections
//! with `[` and `]`, so a test reads as before and after.

use text_core::editing::{self, bracket_pair};
use text_core::{
    Buffer, EditSource, FileFormat, RopeStore, Selection, SelectionSet, TextStore, commands,
};

const USER: EditSource = EditSource::User;

/// Builds a buffer from text with `|` carets or `[`..`]` selections.
fn buffer(marked: &str) -> Buffer<RopeStore> {
    let mut text = String::new();
    let mut selections = Vec::new();
    let mut open = None;
    for character in marked.chars() {
        match character {
            '|' => selections.push(Selection::caret(text.len())),
            '[' => open = Some(text.len()),
            ']' => selections.push(Selection::new(open.take().unwrap(), text.len())),
            other => text.push(other),
        }
    }
    let mut buffer = Buffer::new(&text, FileFormat::default());
    buffer.set_selections(SelectionSet::from_vec(selections));
    buffer
}

/// The text with the selections marked again.
fn shown(buffer: &Buffer<RopeStore>) -> String {
    let text = buffer.text();
    let mut marks: Vec<(usize, char)> = Vec::new();
    for selection in buffer.selections().iter() {
        if selection.is_empty() {
            marks.push((selection.head, '|'));
        } else {
            marks.push((selection.start(), '['));
            marks.push((selection.end(), ']'));
        }
    }
    marks.sort_by_key(|(at, mark)| (*at, *mark != ']'));
    let mut out = String::new();
    let mut last = 0;
    for (at, mark) in marks {
        out.push_str(&text[last..at]);
        out.push(mark);
        last = at;
    }
    out.push_str(&text[last..]);
    out
}

fn typed(marked: &str, character: char) -> String {
    let mut buffer = buffer(marked);
    editing::type_char(&mut buffer, character, "    ", USER);
    shown(&buffer)
}

#[test]
fn an_opening_bracket_brings_its_partner() {
    assert_eq!(typed("foo|", '('), "foo(|)");
    assert_eq!(typed("|", '{'), "{|}");
    assert_eq!(typed("a| b", '['), "a[|] b");
}

#[test]
fn a_bracket_before_a_name_stays_alone() {
    assert_eq!(typed("|name", '('), "(|name");
}

#[test]
fn a_closing_character_is_typed_over() {
    assert_eq!(typed("f(a|)", ')'), "f(a)|");
    assert_eq!(typed("\"a|\"", '"'), "\"a\"|");
}

#[test]
fn quotes_pair_up_except_after_a_word() {
    assert_eq!(typed("x = |", '"'), "x = \"|\"");
    assert_eq!(typed("don|", '\''), "don'|");
}

#[test]
fn a_selection_is_wrapped_and_stays_selected() {
    assert_eq!(typed("a [bc] d", '('), "a ([bc]) d");
    assert_eq!(typed("a [bc] d", '"'), "a \"[bc]\" d");
}

#[test]
fn every_caret_types() {
    assert_eq!(typed("a| b|", '('), "a(|) b(|)");
}

#[test]
fn a_closing_brace_at_the_start_of_a_line_takes_back_an_indent() {
    assert_eq!(typed("if (x) {\n    |", '}'), "if (x) {\n}|");
}

#[test]
fn backspace_removes_an_empty_pair() {
    let mut buffer = buffer("f(|)");
    commands::backspace(&mut buffer, USER);
    assert_eq!(shown(&buffer), "f|");
    let mut buffer = buffer_of("f(a|)");
    commands::backspace(&mut buffer, USER);
    assert_eq!(shown(&buffer), "f(|)");
}

fn buffer_of(marked: &str) -> Buffer<RopeStore> {
    buffer(marked)
}

#[test]
fn enter_between_brackets_opens_a_block() {
    let mut buffer = buffer("void f() {|}");
    editing::newline(&mut buffer, |_, _| "    ".to_owned(), USER);
    assert_eq!(shown(&buffer), "void f() {\n    |\n}");
    let mut buffer = buffer_of("    call(|)");
    editing::newline(&mut buffer, |_, _| "        ".to_owned(), USER);
    assert_eq!(shown(&buffer), "    call(\n        |\n    )");
}

#[test]
fn enter_elsewhere_keeps_the_indent() {
    let mut buffer = buffer("  ab|cd");
    editing::newline(&mut buffer, |_, _| "  ".to_owned(), USER);
    assert_eq!(shown(&buffer), "  ab\n  |cd");
}

#[test]
fn a_new_line_below_and_above() {
    let mut below = buffer("  a|b\nc");
    editing::start_line(&mut below, false, |_, _| "  ".to_owned(), USER);
    assert_eq!(shown(&below), "  ab\n  |\nc");
    let mut above = buffer("x\n  a|b");
    editing::start_line(&mut above, true, |_, _| String::new(), USER);
    assert_eq!(shown(&above), "x\n  |\n  ab");
}

#[test]
fn join_takes_the_indent_of_the_next_line() {
    let mut buffer = buffer("a|\n    b\nc");
    editing::join_lines(&mut buffer, USER);
    assert_eq!(shown(&buffer), "a| b\nc");
}

#[test]
fn join_covers_a_selection() {
    let mut buffer = buffer("[a\nb\nc]\nd");
    editing::join_lines(&mut buffer, USER);
    assert_eq!(buffer.text(), "a b c\nd");
}

#[test]
fn join_at_the_last_line_does_nothing() {
    let mut buffer = buffer("a\nb|");
    let change = editing::join_lines(&mut buffer, USER);
    assert!(change.is_empty());
}

#[test]
fn case_toggles_a_selection_or_the_word_at_the_caret() {
    let mut buffer = buffer("[Hello] world");
    editing::toggle_case(&mut buffer, USER);
    assert_eq!(shown(&buffer), "[HELLO] world");
    editing::toggle_case(&mut buffer, USER);
    assert_eq!(shown(&buffer), "[hello] world");
    let mut word = buffer_of("say wor|ld");
    editing::toggle_case(&mut word, USER);
    assert_eq!(shown(&word), "say WOR|LD");
}

#[test]
fn a_word_is_deleted_backwards_and_forwards() {
    let mut back = buffer("foo bar|");
    editing::delete_word(&mut back, false, USER);
    assert_eq!(shown(&back), "foo |");
    editing::delete_word(&mut back, false, USER);
    assert_eq!(shown(&back), "foo|");
    let mut forward = buffer_of("|foo bar");
    editing::delete_word(&mut forward, true, USER);
    assert_eq!(shown(&forward), "| bar");
}

#[test]
fn deleting_a_word_stops_at_the_line_end() {
    let mut back = buffer("a\n|b");
    editing::delete_word(&mut back, false, USER);
    assert_eq!(shown(&back), "a|b");
    let mut punctuation = buffer_of("call(|");
    editing::delete_word(&mut punctuation, false, USER);
    assert_eq!(shown(&punctuation), "call|");
}

#[test]
fn a_block_comment_wraps_and_unwraps() {
    let marks = ("/*", "*/");
    let mut buffer = buffer("a [b c] d");
    editing::toggle_block_comment(&mut buffer, marks, USER);
    assert_eq!(buffer.text(), "a /*b c*/ d");
    let mut again = buffer_of("a [/*b c*/] d");
    editing::toggle_block_comment(&mut again, marks, USER);
    assert_eq!(again.text(), "a b c d");
}

#[test]
fn a_block_comment_without_selection_takes_the_line() {
    let mut buffer = buffer("  foo(|)  ");
    editing::toggle_block_comment(&mut buffer, ("/*", "*/"), USER);
    assert_eq!(buffer.text(), "  /*foo()*/  ");
}

#[test]
fn shift_tab_takes_an_indent_off_every_line() {
    let mut buffer = buffer("[    a\n  b\nc]");
    editing::unindent(&mut buffer, "    ", USER);
    assert_eq!(buffer.text(), "a\nb\nc");
}

#[test]
fn lines_get_the_indent_they_are_asked_for() {
    let mut buffer = buffer("[a\n      b\n\nc]");
    editing::reindent(&mut buffer, |_, line| "  ".repeat(line.min(2)), USER);
    assert_eq!(buffer.text(), "a\n  b\n\n    c");
}

#[test]
fn all_occurrences_of_a_word_are_selected() {
    let mut buffer = buffer("pe|t petal pet");
    editing::select_all_occurrences(&mut buffer);
    assert_eq!(shown(&buffer), "[pet] petal [pet]");
}

#[test]
fn all_occurrences_of_a_selection_include_parts_of_words() {
    let mut buffer = buffer("[pet] petal");
    editing::select_all_occurrences(&mut buffer);
    assert_eq!(buffer.selections().len(), 2);
}

#[test]
fn a_bracket_finds_its_partner() {
    let store = RopeStore::from_text("f(a, [b]) { x }");
    assert_eq!(bracket_pair(&store, 1), Some((1, 8)));
    assert_eq!(bracket_pair(&store, 9), Some((1, 8)));
    assert_eq!(bracket_pair(&store, 4), None);
    assert_eq!(bracket_pair(&store, 10), Some((10, 14)));
}

#[test]
fn brackets_in_strings_and_comments_do_not_count() {
    let store = RopeStore::from_text("f(\")\" /* ) */ // )\n)");
    assert_eq!(bracket_pair(&store, 1), Some((1, 19)));
    assert_eq!(bracket_pair(&store, 20), Some((1, 19)));
}

#[test]
fn unbalanced_brackets_have_no_partner() {
    let store = RopeStore::from_text("(]");
    assert_eq!(bracket_pair(&store, 0), None);
}

const WORDS: &[&str] = &["if", "else", "for", "while", "class"];

fn completed(marked: &str) -> String {
    let mut buffer = buffer(marked);
    editing::complete_statement(&mut buffer, ";", WORDS, "    ", USER);
    shown(&buffer)
}

#[test]
fn a_plain_statement_gets_its_terminator_and_a_new_line() {
    assert_eq!(completed("    int x = 5|"), "    int x = 5;\n    |");
}

#[test]
fn open_brackets_are_closed_first() {
    assert_eq!(completed("call(a, b(|"), "call(a, b());\n|");
    assert_eq!(completed("say(\"(\"|"), "say(\"(\");\n|");
}

#[test]
fn a_statement_with_a_block_gets_braces() {
    assert_eq!(completed("  if (ready|)"), "  if (ready) {\n      |\n  }");
    assert_eq!(completed("while (a < b|"), "while (a < b) {\n    |\n}");
    assert_eq!(completed("class A|"), "class A {\n    |\n}");
}

#[test]
fn an_open_brace_gets_its_body_and_a_finished_line_just_a_new_line() {
    assert_eq!(completed("else {|"), "else {\n    |\n}");
    assert_eq!(completed("  a();|"), "  a();\n  |");
}
