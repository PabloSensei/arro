//! The type of the expression at the caret, Ctrl+Shift+P. A literal has the type of its kind,
//! read from the text. A name, a field or a call takes the type that the hover answer of the
//! server gives: the type of a variable, a parameter or a field, or the return type of a method.
//! The packages of the names are left out, as IDEA shows them.

use std::ops::Range;

use java_syntax::{ExpressionShape, LiteralKind};
use lang_api::ExpressionTypePlace;

/// Where the type of the expression that covers `expression`, a byte range of `text`, is read
/// from. `None` when the expression is not one the language gives a type for.
pub(crate) fn place(text: &str, expression: Range<usize>) -> Option<ExpressionTypePlace> {
    match java_syntax::expression_shape(text, expression)? {
        ExpressionShape::Literal(kind) => {
            Some(ExpressionTypePlace::Known(literal_type(kind).to_owned()))
        }
        ExpressionShape::Named(offset) => Some(ExpressionTypePlace::Hover(offset)),
        ExpressionShape::Other => None,
    }
}

/// The type that the hover answer gives for the name at `offset` of `text`, in one line. The
/// first line of the answer that is not a code fence declares the name: a variable, a parameter
/// or a field as `Type name`, optionally followed by ` - place`, and a method as
/// `ReturnType container.name(parameters)`, where the container is the qualified class that
/// declares it. The type is what the declaration gives before the name.
pub(crate) fn from_hover(text: &str, offset: usize, answer: &str) -> Option<String> {
    let name = identifier_at(text, offset)?;
    let line = answer
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))?;
    let declaration = line.split_once(" - ").map_or(line, |(head, _)| head);
    let (before, call) = before_name(declaration, name)?;
    let before = before.trim_end();
    let declared = match (call, before.strip_suffix('.')) {
        // A class name is only its package before it: the answer names a type, not a value.
        (false, Some(_)) => return None,
        (false, None) => before,
        // The return type comes before the qualified container of the method.
        (true, Some(container)) => &container[..last_space_at_top_level(container)?],
        (true, None) => before,
    };
    Some(simple_names(strip_modifiers(declared)?))
}

fn literal_type(kind: LiteralKind) -> &'static str {
    match kind {
        LiteralKind::Int => "int",
        LiteralKind::Long => "long",
        LiteralKind::Float => "float",
        LiteralKind::Double => "double",
        LiteralKind::Char => "char",
        LiteralKind::String => "String",
        LiteralKind::Boolean => "boolean",
        LiteralKind::Null => "<null>",
    }
}

/// The identifier that starts at `offset`, or `None` when no identifier starts there.
fn identifier_at(text: &str, offset: usize) -> Option<&str> {
    let rest = text.get(offset..)?;
    let end = rest
        .find(|character: char| !is_identifier_part(character))
        .unwrap_or(rest.len());
    (end > 0 && rest.starts_with(is_identifier_start)).then(|| &rest[..end])
}

fn is_identifier_start(character: char) -> bool {
    character.is_alphabetic() || character == '_' || character == '$'
}

fn is_identifier_part(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// The text before the name in the declaration, and whether the name is followed by a
/// parenthesis, so that it names a method. The name is the first whole word of the declaration
/// outside the brackets that is followed by the end of the declaration or by a parenthesis.
fn before_name<'a>(declaration: &'a str, name: &str) -> Option<(&'a str, bool)> {
    let mut from = 0;
    while let Some(found) = declaration[from..].find(name) {
        let start = from + found;
        let end = start + name.len();
        from = end;
        let whole = !declaration[..start]
            .chars()
            .next_back()
            .is_some_and(is_identifier_part)
            && !declaration[end..]
                .chars()
                .next()
                .is_some_and(is_identifier_part);
        let after = declaration[end..].trim_start();
        let call = after.starts_with('(');
        if whole && (call || after.is_empty()) && depth_at(declaration, start) == 0 {
            return Some((&declaration[..start], call));
        }
    }
    None
}

/// The number of brackets open before `index`: angle brackets, parentheses and square brackets.
fn depth_at(text: &str, index: usize) -> usize {
    let mut depth: usize = 0;
    for character in text[..index].chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

/// The index of the last space of `text` outside the brackets, or `None` when there is none.
fn last_space_at_top_level(text: &str) -> Option<usize> {
    let mut depth: usize = 0;
    let mut space = None;
    for (index, character) in text.char_indices() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            _ if character.is_whitespace() && depth == 0 => space = Some(index),
            _ => {}
        }
    }
    space
}

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "synchronized",
    "native",
    "transient",
    "volatile",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
];

/// The type left of a declaration once its annotations, its modifiers and the type parameters
/// of a generic method are gone. `None` for a declaration that is no type, such as a class.
fn strip_modifiers(before: &str) -> Option<&str> {
    let mut rest = before.trim();
    loop {
        if let Some(generic) = rest.strip_prefix('<') {
            let close = closing(generic, '<', '>')?;
            rest = generic[close..].trim_start();
        } else if let Some(annotation) = rest.strip_prefix('@') {
            if annotation.starts_with("interface") {
                return None;
            }
            let name_end = annotation
                .find(|character: char| character.is_whitespace() || character == '(')
                .unwrap_or(annotation.len());
            let mut after = &annotation[name_end..];
            if let Some(arguments) = after.strip_prefix('(') {
                after = arguments[closing(arguments, '(', ')')?..].trim_start();
            }
            rest = after.trim_start();
        } else {
            let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            if !MODIFIERS.contains(&&rest[..word_end]) {
                break;
            }
            rest = rest[word_end..].trim_start();
        }
    }
    if rest.is_empty()
        || matches!(
            rest.split_whitespace().next(),
            Some("class" | "interface" | "enum" | "record")
        )
    {
        return None;
    }
    Some(rest)
}

/// The index just after the bracket that closes the one `text` starts inside of, or `None`
/// when it never closes.
fn closing(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 1usize;
    for (index, character) in text.char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                return Some(index + character.len_utf8());
            }
        }
    }
    None
}

/// Shortens every dotted name of `text` to the classes it names: the packages go, and a class
/// keeps the classes around it. `java.util.Map.Entry<String, org.example.Owner>` becomes
/// `Map.Entry<String, Owner>`.
fn simple_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_identifier_start) {
        out.push_str(&rest[..start]);
        let length = qualified_length(&rest[start..]);
        let segments: Vec<&str> = rest[start..start + length].split('.').collect();
        out.push_str(&class_part(&segments));
        rest = &rest[start + length..];
    }
    out.push_str(rest);
    out
}

/// The byte length of the dotted name that starts `text`: identifiers joined by dots.
fn qualified_length(text: &str) -> usize {
    let mut end = segment_length(text);
    while text[end..].starts_with('.') && text[end + 1..].starts_with(is_identifier_start) {
        end += 1 + segment_length(&text[end + 1..]);
    }
    end
}

fn segment_length(text: &str) -> usize {
    text.find(|character: char| !is_identifier_part(character))
        .unwrap_or(text.len())
}

/// The part of a dotted name that names a class: from the first segment that starts with a
/// capital letter, or the last segment when none does.
fn class_part(segments: &[&str]) -> String {
    match segments
        .iter()
        .position(|segment| segment.starts_with(char::is_uppercase))
    {
        Some(index) => segments[index..].join("."),
        None => segments.last().copied().unwrap_or_default().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use lang_api::ExpressionTypePlace;

    use super::{from_hover, place};

    /// The byte of `part` inside the first `needle` of the text.
    fn at(text: &str, needle: &str, part: &str) -> usize {
        text.find(needle).expect("the needle is in the text") + needle.find(part).unwrap()
    }

    /// The first line of a server answer as the window gets it: a code fence, the declaration,
    /// the fence again and the text under it.
    fn fenced(declaration: &str) -> String {
        format!("```java\n{declaration}\n```\n\nSource: *[spring-petclinic-project]*")
    }

    #[test]
    fn a_parameter_gives_its_declared_type() {
        let text = "class A { void run(Page<Owner> paginated) { paginated.getContent(); } }";
        let offset = at(text, "paginated.getContent", "paginated");
        let answer = fenced(
            "Page<Owner> paginated - org.springframework.samples.petclinic.owner.OwnerController.addPaginationModel(int, Model, Page<Owner>)",
        );
        assert_eq!(
            from_hover(text, offset, &answer).as_deref(),
            Some("Page<Owner>")
        );
    }

    #[test]
    fn a_local_variable_gives_its_type_without_the_place() {
        let text = "class A { void run() { Model model = null; model.addAttribute(\"a\", 1); } }";
        let offset = at(text, "model.addAttribute", "model");
        let answer = fenced(
            "Model model - org.springframework.samples.petclinic.owner.OwnerController.addPaginationModel(int, Model, Page<Owner>)",
        );
        assert_eq!(from_hover(text, offset, &answer).as_deref(), Some("Model"));
    }

    #[test]
    fn a_field_gives_its_type_as_the_server_writes_it() {
        let text = "class A { private final OwnerRepository owners; void run() { this.owners.save(null); } }";
        let offset = at(text, "this.owners.save", "owners");
        let answer = fenced("OwnerRepository owners");
        assert_eq!(
            from_hover(text, offset, &answer).as_deref(),
            Some("OwnerRepository")
        );
    }

    #[test]
    fn a_field_drops_the_packages_of_its_type() {
        let text = "class A { void run() { owners.size(); } }";
        let offset = at(text, "owners.size", "owners");
        let answer =
            "java.util.List<org.example.owner.Owner> owners - org.example.owner.OwnerController";
        assert_eq!(
            from_hover(text, offset, answer).as_deref(),
            Some("List<Owner>")
        );
    }

    #[test]
    fn a_method_gives_its_return_type_and_leaves_out_its_container() {
        let text = "class A { void run(Page<Owner> paginated) { List<Owner> listOwners = paginated.getContent(); } }";
        let offset = at(text, "paginated.getContent", "getContent");
        let answer = fenced("List<Owner> org.springframework.data.domain.Slice.getContent()");
        assert_eq!(
            from_hover(text, offset, &answer).as_deref(),
            Some("List<Owner>")
        );
    }

    #[test]
    fn a_static_method_gives_its_return_type() {
        let text =
            "class A { void run(int validatedPage) { PageRequest.of(validatedPage - 1, 5); } }";
        let offset = at(text, "PageRequest.of", "of");
        let answer = fenced(
            "PageRequest org.springframework.data.domain.PageRequest.of(int pageNumber, int pageSize)",
        );
        assert_eq!(
            from_hover(text, offset, &answer).as_deref(),
            Some("PageRequest")
        );
    }

    #[test]
    fn a_generic_method_gives_its_return_type_without_its_type_parameters() {
        let text = "class A { void run() { Lists.pick(items); } }";
        let offset = at(text, "Lists.pick", "pick");
        let answer = "<T> List<T> org.example.Lists.pick(List<T> items)";
        assert_eq!(from_hover(text, offset, answer).as_deref(), Some("List<T>"));
    }

    #[test]
    fn a_type_name_gives_no_type() {
        let text = "class A { void run(Owner owner) { owner.save(); } }";
        let offset = at(text, "Owner owner", "Owner");
        let answer = fenced("org.springframework.samples.petclinic.owner.Owner");
        assert_eq!(from_hover(text, offset, &answer), None);
    }

    #[test]
    fn a_constructor_without_a_return_type_gives_no_type() {
        let text = "class A { Owner make() { return new Owner(); } }";
        let offset = at(text, "new Owner()", "Owner");
        let answer = "org.example.owner.Owner.Owner()";
        assert_eq!(from_hover(text, offset, answer), None);
    }

    #[test]
    fn a_nested_type_keeps_its_class_and_drops_the_packages() {
        let text = "class A { void run() { entry.getKey(); } }";
        let offset = at(text, "entry.getKey", "entry");
        let answer = "java.util.Map.Entry<java.lang.String, org.example.owner.Owner> entry - org.example.A.run()";
        assert_eq!(
            from_hover(text, offset, answer).as_deref(),
            Some("Map.Entry<String, Owner>")
        );
    }

    #[test]
    fn an_answer_that_does_not_declare_the_name_gives_no_type() {
        let text = "class A { void run(Owner owner) { owner.getName(); } }";
        let offset = at(text, "owner.getName", "owner");
        assert_eq!(
            from_hover(text, offset, "String other - org.example.A.run(Owner)"),
            None
        );
    }

    #[test]
    fn a_literal_has_the_type_of_its_kind() {
        let text = "class A { void run() { int a = 10; long b = 10L; double c = 1.5; float d = 1.5f; char e = 'x'; String f = \"s\"; boolean g = true; Object h = null; } }";
        let cases = [
            ("10;", "10", "int"),
            ("10L", "10L", "long"),
            ("1.5;", "1.5", "double"),
            ("1.5f", "1.5f", "float"),
            ("'x'", "'x'", "char"),
            ("\"s\"", "\"s\"", "String"),
            ("true", "true", "boolean"),
            ("null", "null", "<null>"),
        ];
        for (needle, part, expected) in cases {
            let start = at(text, needle, part);
            assert_eq!(
                place(text, start..start + part.len()),
                Some(ExpressionTypePlace::Known(expected.to_owned())),
                "the literal {part}"
            );
        }
    }

    #[test]
    fn a_declaration_is_no_expression_and_gives_no_place() {
        let text = "class A { void run() { int a = 10; } }";
        let start = at(text, "a = 10", "a");
        assert_eq!(place(text, start..start + 1), None);
    }
}
