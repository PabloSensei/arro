//! Folding from the header lines of the text: the keys of a member body, a block and
//! Collapse All, on a source whose bodies open from their declarations.

use std::ops::Range;

use display_map::Fold;
use keymap::editor as actions;
use text_core::FileFormat;

use super::EditorState;

/// The declarations of a type: a short signature, an annotated one, a signature that
/// wraps and a long one that stays on a line, with a block inside two of the bodies.
const SAMPLE: &str = r#"class OwnerController {
    @InitBinder
    public void setAllowedFields(WebDataBinder dataBinder) {
        dataBinder.setDisallowedFields("id");
    }

    public String processCreationForm(Owner owner,
            BindingResult result) {
        if (result.hasErrors()) {
            return "createOrUpdateOwnerForm";
        }
        return "redirect:/owners";
    }

    public Page<Owner> findPaginatedForOwnersLastName(int page, String lastname) {
        return repository.findAll(page, lastname);
    }

    public String processFindForm(Owner owner, BindingResult result, Model model,
            ModelAndView mav) {
        if (owner.getLastName() == null) {
            owner.setLastName("");
        }
        return "owners/findOwners";
    }
}
"#;

/// A state whose regions are the `{...}` pairs of the text, the way a parser reports the
/// bodies of the members and the blocks inside them.
fn state(text: &str) -> EditorState {
    let mut state = EditorState::new(text, FileFormat::default(), None, false);
    let folds = super::brace_pairs(text)
        .into_iter()
        .map(|(open, close)| Fold {
            range: open..close + 1,
            placeholder: "{...}".to_owned(),
        })
        .collect();
    state.map.folds.set_candidates(folds);
    state
}

fn line_offset(text: &str, line: usize) -> usize {
    text.split_inclusive('\n').take(line).map(str::len).sum()
}

/// The byte of the `{` that opens the next body after `header`.
fn body_start(text: &str, header: &str) -> usize {
    let from = text.find(header).expect("the header is in the sample");
    text[from..].find('{').expect("the header opens a body") + from
}

fn body_at(text: &str, start: usize) -> Range<usize> {
    let (_, close) = super::brace_pairs(text)
        .into_iter()
        .find(|(open, _)| *open == start)
        .expect("the body closes");
    start..close + 1
}

fn caret(state: &mut EditorState, offset: usize) {
    state.select_range(offset..offset);
}

fn folds(state: &EditorState) -> Vec<Range<usize>> {
    state
        .map()
        .folds
        .as_slice()
        .iter()
        .map(|fold| fold.range.clone())
        .collect()
}

fn shown(state: &EditorState) -> String {
    let display = state.display();
    (0..display.row_count())
        .map(|row| display.line(row).text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_signature_line_folds_the_body_that_opens_on_it() {
    let mut state = state(SAMPLE);
    let rows = state.display().row_count();
    let body = body_at(SAMPLE, body_start(SAMPLE, "void setAllowedFields"));
    let start = line_offset(SAMPLE, 2) + 20;
    caret(&mut state, start);

    state.perform(actions::FOLD, 10);

    assert_eq!(
        folds(&state).as_slice(),
        std::slice::from_ref(&body),
        "only the method body folds"
    );
    assert_eq!(state.caret(), (2, 20), "the caret stays on the header");
    assert!(state.display().row_count() < rows);
    assert!(
        shown(&state).contains("class OwnerController {"),
        "the class stays open"
    );
    assert!(
        shown(&state).contains("public void setAllowedFields"),
        "the signature stays in sight"
    );

    state.perform(actions::UNFOLD, 10);

    assert!(folds(&state).is_empty(), "Ctrl+= opens the body again");
    assert_eq!(state.display().row_count(), rows);
    assert_eq!(state.caret(), (2, 20), "the caret stays on the header");

    state.perform(actions::FOLD, 10);
    state.perform(actions::UNFOLD_ALL, 10);
    assert!(folds(&state).is_empty(), "Ctrl+Shift+= opens every body");
    assert_eq!(state.display().row_count(), rows);
    assert_eq!(state.caret(), (2, 20), "the caret stays on the header");
}

#[test]
fn a_long_signature_folds_its_body_from_the_line() {
    let mut state = state(SAMPLE);
    let body = body_at(SAMPLE, body_start(SAMPLE, "findPaginatedForOwnersLastName"));
    caret(&mut state, line_offset(SAMPLE, 14) + 30);

    state.perform(actions::FOLD, 10);

    assert_eq!(folds(&state), [body]);
    assert_eq!(state.caret(), (14, 30));
    assert!(
        shown(&state).contains("class OwnerController {"),
        "the class stays open"
    );
}

#[test]
fn a_wrapped_signature_folds_and_opens_its_body_from_either_line() {
    let mut state = state(SAMPLE);
    let start = body_start(SAMPLE, "String processCreationForm");
    let body = body_at(SAMPLE, start);

    // The first line of the signature names the body opening on the next one.
    caret(&mut state, line_offset(SAMPLE, 6) + 8);
    state.perform(actions::FOLD, 10);
    assert_eq!(folds(&state).as_slice(), std::slice::from_ref(&body));
    assert_eq!(state.caret(), (6, 8), "the caret stays on the signature");
    state.perform(actions::UNFOLD, 10);
    assert!(folds(&state).is_empty(), "Ctrl+= opens the wrapped body");

    // The line of the `{` itself folds the same body.
    caret(&mut state, start);
    state.perform(actions::FOLD, 10);
    assert_eq!(folds(&state), [body]);
}

#[test]
fn an_annotated_method_folds_from_the_signature_and_the_annotation() {
    let mut state = state(SAMPLE);
    let body = body_at(SAMPLE, body_start(SAMPLE, "void setAllowedFields"));

    caret(&mut state, line_offset(SAMPLE, 2) + 16);
    state.perform(actions::FOLD, 10);
    assert_eq!(folds(&state).as_slice(), std::slice::from_ref(&body));
    state.perform(actions::UNFOLD, 10);

    // The annotation belongs to the header the caret stands on.
    caret(&mut state, line_offset(SAMPLE, 1) + 5);
    state.perform(actions::FOLD, 10);
    assert_eq!(folds(&state), [body]);
}

#[test]
fn a_caret_inside_an_if_folds_that_block() {
    let mut state = state(SAMPLE);
    let inside = body_at(SAMPLE, body_start(SAMPLE, "if (result.hasErrors())"));
    caret(&mut state, line_offset(SAMPLE, 9) + 12);

    state.perform(actions::FOLD, 10);

    assert_eq!(
        folds(&state),
        [inside],
        "the innermost block around the caret"
    );
    assert!(
        shown(&state).contains("class OwnerController {"),
        "the class stays open"
    );
    assert!(
        shown(&state).contains("String processCreationForm"),
        "the method stays open"
    );
}

#[test]
fn collapse_all_folds_every_member_body_and_keeps_the_class_open() {
    let mut state = state(SAMPLE);
    let rows = state.display().row_count();
    let class = body_at(SAMPLE, SAMPLE.find('{').unwrap());
    let bodies = [
        body_at(SAMPLE, body_start(SAMPLE, "void setAllowedFields")),
        body_at(SAMPLE, body_start(SAMPLE, "String processCreationForm")),
        body_at(SAMPLE, body_start(SAMPLE, "findPaginatedForOwnersLastName")),
        body_at(SAMPLE, body_start(SAMPLE, "String processFindForm")),
    ];
    caret(&mut state, line_offset(SAMPLE, 6) + 8);

    state.perform(actions::FOLD_ALL, 10);

    assert_eq!(
        folds(&state),
        bodies,
        "the bodies of every method fold, the short and the wrapped ones"
    );
    assert!(!folds(&state).contains(&class), "the class body stays open");
    assert_eq!(
        state.caret(),
        (6, 8),
        "the caret stays on the header of a member"
    );
    let text = shown(&state);
    assert!(text.contains("class OwnerController {"), "{text}");
    assert!(text.contains("public void setAllowedFields"), "{text}");
    assert!(text.contains("String processCreationForm"), "{text}");
    assert!(text.contains("String processFindForm"), "{text}");
    assert!(
        !text.contains("return \"redirect:/owners\""),
        "a body of a method with a block inside hides too: {text}"
    );

    state.perform(actions::UNFOLD_ALL, 10);

    assert!(folds(&state).is_empty(), "Expand All opens every body");
    assert_eq!(state.display().row_count(), rows);
    assert_eq!(state.caret(), (6, 8), "the caret stays where it was");
}
