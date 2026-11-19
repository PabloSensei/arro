//! Complete Statement: the plan of the edit for the statements of the grammar.

/// Applies the plan of Complete Statement at `caret` to `text` and answers the new text
/// with the place of the caret in it.
fn finish(text: &str, caret: usize) -> (String, usize) {
    let edit = java_syntax::complete_statement(text, caret, "    ").expect("a plan");
    let mut new = String::with_capacity(text.len() + edit.text.len());
    new.push_str(&text[..edit.start]);
    new.push_str(&edit.text);
    new.push_str(&text[edit.end..]);
    (new, edit.start + edit.caret)
}

/// The place right after the first `piece`, which the tests write as the caret.
fn after(text: &str, piece: &str) -> usize {
    text.find(piece).expect("the piece of the test") + piece.len()
}

#[test]
fn an_open_call_takes_its_bracket_and_the_terminator() {
    const TEXT: &str = "class A {\n    void run() {\n        foo(a, b\n    }\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "foo(a, b"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, b);\n        \n    }\n}\n"
    );
    assert_eq!(caret, text.find("\n        \n").unwrap() + 9);
}

#[test]
fn a_call_inside_a_call_closes_both_brackets() {
    const TEXT: &str = "class A {\n    void run() {\n        foo(bar(\n    }\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "foo(bar("));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(bar());\n        \n    }\n}\n"
    );
    assert_eq!(caret, text.find(");\n").unwrap() + 3 + 8);
}

#[test]
fn a_condition_takes_its_bracket_and_an_empty_body() {
    const TEXT: &str = "class A {\n    void run() {\n        if (x\n    }\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "if (x"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        if (x) {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "        if (x) {\n") + 12);

    for head in ["while (x", "for (int i = 0; i < n", "switch (x"] {
        let text = format!("class A {{\n    void run() {{\n        {head}\n    }}\n}}\n");
        let (done, _) = finish(&text, after(&text, head));
        let word = head.split(' ').next().unwrap();
        assert!(
            done.contains(&format!("{word} (")),
            "{head} stays a header: {done:?}"
        );
        assert!(done.contains(") {"), "{head} gets its bracket: {done:?}");
        assert!(
            done.contains("\n            \n"),
            "{head} gets a body: {done:?}"
        );
    }
}

#[test]
fn a_method_header_takes_its_body() {
    const TEXT: &str = "class A {\n    void run(\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "void run("));
    assert_eq!(text, "class A {\n    void run() {\n        \n    }\n}\n");
    assert_eq!(caret, after(&text, "    void run() {\n") + 8);

    const PARAMS: &str = "class A {\n    int sum(int a\n}\n";
    let (text, _) = finish(PARAMS, after(PARAMS, "int sum(int a"));
    assert_eq!(
        text,
        "class A {\n    int sum(int a) {\n        \n    }\n}\n"
    );
}

#[test]
fn else_catch_and_try_take_their_blocks() {
    const ELSE: &str = "class A {\n    void run() {\n        if (a) {\n        } else\n    }\n}\n";
    let (text, caret) = finish(ELSE, after(ELSE, "} else"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        if (a) {\n        } else {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "} else {\n") + 12);

    const TRY: &str = "class A {\n    void run() {\n        try\n    }\n}\n";
    let (text, _) = finish(TRY, after(TRY, "try"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        try {\n            \n        }\n    }\n}\n"
    );

    // `catch` alone gets an empty parameter list with the caret between its brackets.
    const CATCH: &str = "class A {\n    void run() {\n        try {\n        } catch\n    }\n}\n";
    let (text, caret) = finish(CATCH, after(CATCH, "} catch"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        try {\n        } catch () {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(caret, text.find("catch (").unwrap() + "catch (".len());

    const TYPED: &str =
        "class A {\n    void run() {\n        try {\n        } catch (IOException\n    }\n}\n";
    let (text, caret) = finish(TYPED, after(TYPED, "catch (IOException"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        try {\n        } catch (IOException) {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "(IOException) {\n") + 12);
}

#[test]
fn a_finished_statement_only_starts_the_next_line() {
    const TEXT: &str = "class A {\n    void run() {\n        foo(a, b);\n    }\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "foo(a, b);"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, b);\n        \n    }\n}\n"
    );
    assert_eq!(caret, after(&text, ";\n") + 8);

    // A call without its terminator gets one.
    const OPEN: &str = "class A {\n    void run() {\n        foo(a, b)\n    }\n}\n";
    let (text, _) = finish(OPEN, after(OPEN, "foo(a, b)"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, b);\n        \n    }\n}\n"
    );

    // A declaration without its terminator gets one too.
    const LOCAL: &str = "class A {\n    void run() {\n        int x = 1\n    }\n}\n";
    let (text, _) = finish(LOCAL, after(LOCAL, "int x = 1"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        int x = 1;\n        \n    }\n}\n"
    );
}

#[test]
fn a_class_header_and_a_comment_take_the_lines_of_the_language() {
    const CLASS: &str = "class A\n";
    let (text, _) = finish(CLASS, after(CLASS, "class A"));
    assert_eq!(text, "class A {\n    \n}\n");

    const NOTE: &str = "class A {\n    void run() {\n        // note\n    }\n}\n";
    let (text, _) = finish(NOTE, after(NOTE, "// note"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        // note\n        \n    }\n}\n"
    );
}

#[test]
fn a_statement_of_two_lines_closes_where_the_caret_is() {
    const TEXT: &str = "class A {\n    void run() {\n        foo(a,\n            b\n    }\n}\n";
    let (text, _) = finish(TEXT, after(TEXT, "            b"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a,\n            b);\n            \n    }\n}\n"
    );
}

#[test]
fn the_marks_of_a_line_go_in_front_of_its_comment() {
    const LINE: &str = "class A {\n    void run() {\n        foo(a, b // note\n    }\n}\n";
    let (text, caret) = finish(LINE, after(LINE, "foo(a, b"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, b); // note\n        \n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "// note\n") + 8);

    const BLOCK: &str = "class A {\n    void run() {\n        foo(a, b /* note */\n    }\n}\n";
    let (text, _) = finish(BLOCK, after(BLOCK, "foo(a, b"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, b); /* note */\n        \n    }\n}\n"
    );

    // A `//` inside a string is code, not the start of a comment, and a slash of the
    // code itself is not a comment either.
    const URL: &str = "class A {\n    void run() {\n        foo(a, \"http://x\"\n    }\n}\n";
    let (text, _) = finish(URL, after(URL, "foo(a, \"http://x\""));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a, \"http://x\");\n        \n    }\n}\n"
    );

    const HALF: &str = "class A {\n    void run() {\n        foo(a / b // note\n    }\n}\n";
    let (text, _) = finish(HALF, after(HALF, "foo(a / b"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        foo(a / b); // note\n        \n    }\n}\n"
    );
}

#[test]
fn a_header_of_two_lines_takes_its_body_as_one_line_does() {
    const IF: &str =
        "class A {\n    void run() {\n        if (x\n                && y)\n    }\n}\n";
    let (text, caret) = finish(IF, after(IF, "&& y)"));
    assert_eq!(
        text,
        "class A {\n    void run() {\n        if (x\n                && y) {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "&& y) {\n            "));

    for head in [
        "while (x\n                && y)",
        "for (; x\n                && y;)",
    ] {
        let text = format!("class A {{\n    void run() {{\n        {head}\n    }}\n}}\n");
        let (done, caret) = finish(&text, after(&text, head));
        let word = head.split(' ').next().unwrap();
        assert!(
            done.contains(&format!("{word} (")),
            "{head} stays a header: {done:?}"
        );
        assert!(
            done.contains(") {\n            \n        }"),
            "{head} gets a body at the indent of its first line: {done:?}"
        );
        assert!(
            caret > after(&done, ") {\n"),
            "{head} puts the caret in the body"
        );
    }
}

#[test]
fn a_method_header_with_parameters_on_two_lines_takes_its_body() {
    const TEXT: &str = "class A {\n    int sum(int a,\n            int b)\n}\n";
    let (text, caret) = finish(TEXT, after(TEXT, "int b)"));
    assert_eq!(
        text,
        "class A {\n    int sum(int a,\n            int b) {\n        \n    }\n}\n"
    );
    assert_eq!(caret, after(&text, "int b) {\n        "));
}

#[test]
fn a_line_without_code_has_no_plan() {
    assert_eq!(
        java_syntax::complete_statement("class A {\n    \n}\n", 12, "    "),
        None
    );
}
