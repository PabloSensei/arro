//! Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
//! Object, computed from the tree of the source files. The window sends the members
//! the user ticked and gets back the edits of every file they touch.

use std::ops::Range;

use lang_api::{
    DocumentEdit, MemberActionForm, MemberChoice, MemberSelection, MemberTarget, MoveMember,
    MovePlace, SourceEdit, TextPoint, TextSpan, WorkspaceChange,
};
use tree_sitter::{Node, Parser, Tree};

const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

const SKIPPED_FOLDERS: &[&str] = &[".git", ".idea", "target", "build", "out", "node_modules"];

/// A source file the form or the edits work on.
#[derive(Clone, Debug)]
pub(crate) struct SourceFile {
    pub uri: String,
    pub text: String,
}

/// A supertype or a subtype with its file.
#[derive(Clone, Debug)]
pub(crate) struct TypeFile {
    pub name: String,
    pub uri: String,
    pub text: String,
    pub package: Option<String>,
}

/// A call of a method in one file, with the places of its name.
#[derive(Clone, Debug)]
pub(crate) struct CallSite {
    pub uri: String,
    pub text: String,
    pub offsets: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberKind {
    Field,
    Method,
    Constructor,
}

#[derive(Clone, Debug)]
struct MemberInfo {
    kind: MemberKind,
    name: String,
    detail: String,
    text: String,
    range: Range<usize>,
    name_range: Range<usize>,
    body: Option<Range<usize>>,
    signature: String,
    private: bool,
    static_: bool,
    abstract_: bool,
}

#[derive(Clone, Debug)]
struct ParsedType {
    name: String,
    name_range: Range<usize>,
    node_range: Range<usize>,
    body_range: Range<usize>,
    header_range: Range<usize>,
    package: Option<String>,
    imports: Vec<String>,
    supertypes: Vec<String>,
    members: Vec<MemberInfo>,
    interface: bool,
    text: String,
}

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&java_syntax::config().grammar).ok()?;
    parser.parse(text, None)
}

fn text_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    &text[node.byte_range()]
}

fn node_at(tree: &Tree, offset: usize) -> Node<'_> {
    let offset = offset.min(tree.root_node().end_byte());
    tree.root_node()
        .descendant_for_byte_range(offset, offset)
        .or_else(|| {
            offset
                .checked_sub(1)
                .and_then(|before| tree.root_node().descendant_for_byte_range(before, before))
        })
        .unwrap_or(tree.root_node())
}

fn type_declaration(node: Node<'_>) -> Option<Node<'_>> {
    let mut node = node;
    loop {
        if TYPE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

fn first_type(root: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .find(|child| TYPE_KINDS.contains(&child.kind()))
}

fn name_of(text: &str, node: Node<'_>) -> Option<String> {
    node.child_by_field_name("name")
        .map(|name| text_of(text, name).to_owned())
}

fn body_of(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("body")
}

/// A package line without its keyword and semicolon.
fn package_of(text: &str, root: Node<'_>) -> Option<String> {
    let mut cursor = root.walk();
    let declaration = root
        .named_children(&mut cursor)
        .find(|child| child.kind() == "package_declaration")?;
    let raw = text_of(text, declaration).trim();
    let name = raw
        .strip_prefix("package")?
        .trim()
        .trim_end_matches(';')
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn imports_of(text: &str, root: Node<'_>) -> Vec<String> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|child| child.kind() == "import_declaration")
        .map(|child| text_of(text, child).trim().to_owned())
        .collect()
}

/// The name of a type without generics and package: `java.util.List<A>` is `List`.
fn simple_name(raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.split('<').next().unwrap_or(raw);
    raw.rsplit('.').next().unwrap_or(raw).trim().to_owned()
}

fn supertypes_of(text: &str, node: Node<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "superclass" => {
                if let Some(name) = child
                    .named_children(&mut child.walk())
                    .next()
                    .map(|found| simple_name(text_of(text, found)))
                    .filter(|name| name != "Object")
                {
                    names.push(name);
                }
            }
            "super_interfaces" | "extends_interfaces" => {
                let mut lists = child.walk();
                for list in child.named_children(&mut lists) {
                    let mut items = list.walk();
                    for item in list.named_children(&mut items) {
                        let name = simple_name(text_of(text, item));
                        if !name.is_empty() {
                            names.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    names
}

fn modifiers_of<'a>(text: &'a str, node: Node<'_>) -> &'a str {
    node.child_by_field_name("modifiers")
        .or_else(|| {
            node.named_children(&mut node.walk())
                .find(|child| child.kind() == "modifiers")
        })
        .map_or("", |modifiers| text_of(text, modifiers))
}

fn has_word(raw: &str, wanted: &str) -> bool {
    raw.split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == wanted)
}

/// The member declarations directly in the body of a type.
fn members_of(text: &str, node: Node<'_>) -> Vec<MemberInfo> {
    let Some(body) = body_of(node) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        let (kind, declaration) = match child.kind() {
            "field_declaration" | "constant_declaration" => (MemberKind::Field, child),
            "method_declaration" => (MemberKind::Method, child),
            "constructor_declaration" | "compact_constructor_declaration" => {
                (MemberKind::Constructor, child)
            }
            _ => continue,
        };
        let name_node = if kind == MemberKind::Field {
            let mut cursor = declaration.walk();
            declaration
                .named_children(&mut cursor)
                .find(|child| child.kind() == "variable_declarator")
                .and_then(|declarator| declarator.child_by_field_name("name"))
        } else {
            declaration.child_by_field_name("name")
        };
        let Some(name) = name_node
            .map(|node| text_of(text, node).to_owned())
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let name_range = name_node.map_or(declaration.byte_range(), |node| node.byte_range());
        let modifiers = modifiers_of(text, declaration);
        let body_node = body_of(declaration);
        let body = body_node.map(|body| body.byte_range());
        let header = body_node.map_or(declaration.byte_range(), |body| {
            declaration.start_byte()..body.start_byte()
        });
        let signature = text[header.clone()].trim().to_owned();
        let detail = match kind {
            MemberKind::Field => declaration
                .child_by_field_name("type")
                .map(|kind| text_of(text, kind).trim().to_owned())
                .unwrap_or_else(|| signature.trim_end_matches(';').trim().to_owned()),
            _ => signature.split_whitespace().collect::<Vec<_>>().join(" "),
        };
        let indent = indent_at(text, declaration.start_byte());
        members.push(MemberInfo {
            kind,
            name,
            detail,
            text: dedent(text_of(text, declaration), &indent),
            range: declaration.byte_range(),
            name_range,
            body,
            signature: signature.trim_end_matches('{').trim().to_owned(),
            private: has_word(modifiers, "private"),
            static_: has_word(modifiers, "static"),
            abstract_: has_word(modifiers, "abstract"),
        });
    }
    members
}

/// The first type of a file with what the member refactorings read from it.
fn parsed(text: &str) -> Option<ParsedType> {
    let tree = parse(text)?;
    let root = tree.root_node();
    let node = first_type(root)?;
    let name_node = node.child_by_field_name("name")?;
    let body = body_of(node)?;
    Some(ParsedType {
        name: text_of(text, name_node).to_owned(),
        name_range: name_node.byte_range(),
        node_range: node.byte_range(),
        body_range: body.byte_range(),
        header_range: node.start_byte()..body.start_byte(),
        package: package_of(text, root),
        imports: imports_of(text, root),
        supertypes: supertypes_of(text, node),
        members: members_of(text, node),
        interface: node.kind() == "interface_declaration"
            || node.kind() == "annotation_type_declaration",
        text: text.to_owned(),
    })
}

/// The type around a byte offset of a file.
fn type_at(text: &str, offset: usize) -> Option<ParsedType> {
    let tree = parse(text)?;
    let node = type_declaration(node_at(&tree, offset))?;
    let file = parsed(text)?;
    // The file functions describe the first type; a nested one is not a place these
    // refactorings work on, so the two agree only for the first type.
    (file.node_range == node.byte_range()).then_some(file)
}

/// What F6 moves at a byte offset of a file: the file, a member the form takes, or a
/// member that this move cannot take.
pub(crate) fn move_place(text: &str, offset: usize) -> MovePlace {
    let Some(tree) = parse(text) else {
        return MovePlace::File;
    };
    let mut node = node_at(&tree, offset);
    loop {
        match node.kind() {
            "constructor_declaration" | "compact_constructor_declaration" => {
                return MovePlace::Unmovable(
                    "A constructor cannot move to another type".to_owned(),
                );
            }
            "static_initializer" => {
                return MovePlace::Unmovable(
                    "A static initializer cannot move to another type".to_owned(),
                );
            }
            "method_declaration" | "field_declaration" | "constant_declaration" => {
                let field = matches!(node.kind(), "field_declaration" | "constant_declaration");
                // The fields of an interface are constants without the modifier.
                let implicit = field
                    && type_declaration(node).is_some_and(|owner| {
                        matches!(
                            owner.kind(),
                            "interface_declaration" | "annotation_type_declaration"
                        )
                    });
                let modifiers = modifiers_of(text, node);
                return if has_word(modifiers, "static") || implicit {
                    MovePlace::Member
                } else {
                    MovePlace::Unmovable("Move Members moves static members only".to_owned())
                };
            }
            kind if TYPE_KINDS.contains(&kind) => return MovePlace::File,
            _ => {}
        }
        let Some(parent) = node.parent() else {
            return MovePlace::File;
        };
        node = parent;
    }
}

/// The members of the type around a byte offset that the Move Members form lists, with
/// the one at the offset ticked. `None` when the type has no member the move can take.
pub(crate) fn move_members(text: &str, offset: usize) -> Option<Vec<MoveMember>> {
    let tree = parse(text)?;
    let node = type_declaration(node_at(&tree, offset))?;
    let implicit = matches!(
        node.kind(),
        "interface_declaration" | "annotation_type_declaration"
    );
    let offset = offset.min(text.len());
    let members: Vec<MoveMember> = members_of(text, node)
        .into_iter()
        .filter(|member| member.static_ || (implicit && member.kind == MemberKind::Field))
        .map(|member| MoveMember {
            checked: member.range.contains(&offset),
            span: TextSpan {
                start: point(text, member.name_range.start),
                end: point(text, member.name_range.end),
            },
            name: member.name,
            detail: Some(member.detail),
        })
        .collect();
    (!members.is_empty()).then_some(members)
}

#[derive(Clone)]
struct Parameter {
    raw: String,
    range: Range<usize>,
    name: String,
    type_name: String,
}

/// The method around a byte offset with what the parameter object refactoring reads.
struct MethodInfo {
    name: String,
    parameters: Vec<Parameter>,
    parameters_range: Range<usize>,
    body: Option<Range<usize>>,
}

fn method_at(text: &str, offset: usize) -> Option<MethodInfo> {
    let tree = parse(text)?;
    let mut node = node_at(&tree, offset);
    loop {
        if node.kind() == "method_declaration" {
            let name = name_of(text, node)?;
            let parameters_node = node.child_by_field_name("parameters")?;
            let mut parameters = Vec::new();
            let mut cursor = parameters_node.walk();
            for child in parameters_node.named_children(&mut cursor) {
                if child.kind() != "formal_parameter" {
                    continue;
                }
                let name = child
                    .child_by_field_name("name")
                    .map(|name| text_of(text, name).to_owned())
                    .unwrap_or_default();
                let type_name = child
                    .child_by_field_name("type")
                    .map(|kind| text_of(text, kind).trim().to_owned())
                    .unwrap_or_default();
                parameters.push(Parameter {
                    raw: text_of(text, child).trim().to_owned(),
                    range: child.byte_range(),
                    name,
                    type_name,
                });
            }
            return Some(MethodInfo {
                name,
                parameters,
                parameters_range: parameters_node.byte_range(),
                body: body_of(node).map(|body| body.byte_range()),
            });
        }
        node = node.parent()?;
    }
}

fn range_value(range: &Range<usize>) -> String {
    format!("{}:{}", range.start, range.end)
}

fn parse_range(value: &str) -> Option<Range<usize>> {
    let (start, end) = value.split_once(':')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    (start < end).then_some(start..end)
}

fn point(text: &str, offset: usize) -> TextPoint {
    let position = lsp_client::utf16::position_at(text, offset.min(text.len()));
    TextPoint {
        line: position.line,
        character: position.character,
    }
}

fn edit(text: &str, range: Range<usize>, new_text: impl Into<String>) -> DocumentEdit {
    DocumentEdit {
        span: TextSpan {
            start: point(text, range.start),
            end: point(text, range.end),
        },
        text: new_text.into(),
    }
}

/// Adds edits of a file to the change, joining the ones of a file that is already
/// there: one source entry per file keeps the change readable.
fn add_edits(all: &mut Vec<SourceEdit>, uri: &str, edits: Vec<DocumentEdit>) {
    if edits.is_empty() {
        return;
    }
    match all.iter_mut().find(|source| source.uri == uri) {
        Some(source) => source.edits.extend(edits),
        None => all.push(SourceEdit {
            uri: uri.to_owned(),
            edits,
        }),
    }
}

/// The whole lines of a range, so a member that leaves takes its blank line with it.
fn full_lines(text: &str, range: &Range<usize>) -> Range<usize> {
    let mut start = range.start;
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    if text[line_start..start].trim().is_empty() {
        start = line_start;
    }
    let mut end = range.end;
    let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at + 1);
    if text[end..line_end].trim().is_empty() {
        end = line_end;
    }
    // A member that had a blank line under it takes that line as well.
    let next_end = text[end..].find('\n').map_or(text.len(), |at| end + at + 1);
    if text[end..next_end].trim().is_empty() {
        end = next_end;
    }
    start..end
}

fn indent_at(text: &str, offset: usize) -> String {
    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    text[line_start..offset.min(text.len())]
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect()
}

/// The indentation a member gets in the body of a type: one level under the line of
/// its declaration.
fn member_indent(text: &str, kind: &ParsedType) -> String {
    let base = indent_at(text, kind.node_range.start);
    format!("{base}    ")
}

/// The text of a declaration with the indentation of its own line taken from every
/// line, so that another place can indent it anew.
fn dedent(text: &str, base: &str) -> String {
    text.lines()
        .map(|line| line.strip_prefix(base).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

fn indent_lines(text: &str, indent: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One document edit that puts the members before the closing brace of the type.
fn insert_members(text: &str, kind: &ParsedType, members: &[String]) -> Option<DocumentEdit> {
    if members.is_empty() {
        return None;
    }
    let closing = kind.body_range.end.checked_sub(1)?;
    let body = &text[kind.body_range.start..closing];
    let indent = member_indent(text, kind);
    let mut inserted = String::new();
    if !body.ends_with('\n') {
        inserted.push('\n');
    }
    for (index, member) in members.iter().enumerate() {
        if index > 0 {
            inserted.push('\n');
        }
        inserted.push_str(&indent_lines(member, &indent));
        inserted.push('\n');
    }
    inserted.push_str(&indent_at(text, kind.node_range.start));
    Some(edit(text, closing..closing, inserted))
}

/// The import lines of `source` that name a type used by `moved`, outside the ones the
/// target already has.
fn needed_imports(source: &ParsedType, moved: &str, target: &ParsedType) -> Vec<String> {
    source
        .imports
        .iter()
        .filter(|line| {
            !target.imports.iter().any(|known| known == *line)
                && line
                    .trim_start_matches("import ")
                    .trim_start_matches("static ")
                    .trim_end_matches(';')
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| has_word(moved, name))
        })
        .cloned()
        .collect()
}

fn imports_edit(text: &str, kind: &ParsedType, imports: &[String]) -> Option<DocumentEdit> {
    if imports.is_empty() {
        return None;
    }
    let mut at = 0;
    if let Some(package) = &kind.package {
        let marker = format!("package {package};");
        at = text.find(&marker).map_or(0, |found| found + marker.len());
    }
    let mut cursor = 0;
    for line in text.lines() {
        let start = cursor;
        cursor += line.len() + 1;
        if line.trim_start().starts_with("import ") {
            at = at.max(start + line.len());
        }
    }
    let mut inserted = String::new();
    for line in imports {
        inserted.push('\n');
        inserted.push_str(line);
    }
    Some(edit(text, at..at, inserted))
}

fn abstract_signature(signature: &str, target_interface: bool) -> String {
    let mut raw = signature.trim().to_owned();
    // Everything that only makes sense with a body.
    for word in [
        "private",
        "static",
        "final",
        "synchronized",
        "native",
        "default",
    ] {
        raw = raw.replace(&format!("{word} "), "");
    }
    if !target_interface && !has_word(&raw, "abstract") {
        let parts: Vec<&str> = raw.splitn(2, char::is_whitespace).collect();
        if parts.len() == 2 && matches!(parts[0], "public" | "protected") {
            raw = format!("{} abstract {}", parts[0], parts[1]);
        } else {
            raw = format!("abstract {raw}");
        }
    }
    raw.trim_end_matches('{').trim().to_owned() + ";"
}

/// The members of a form, all ticked except the private ones a target cannot take.
fn choices_of(kind: &ParsedType) -> Vec<MemberChoice> {
    kind.members
        .iter()
        .filter(|member| member.kind != MemberKind::Constructor)
        .map(|member| MemberChoice {
            name: member.name.clone(),
            detail: Some(member.detail.clone()),
            checked: !member.private,
            can_be_abstract: member.kind == MemberKind::Method
                && member.body.is_some()
                && !member.abstract_,
            abstract_checked: false,
            value: range_value(&member.range),
        })
        .collect()
}

fn private_members(kind: &ParsedType) -> Vec<String> {
    kind.members
        .iter()
        .filter(|member| member.private)
        .map(|member| member.name.clone())
        .collect()
}

/// What the target cannot reach of the members that move: a private member itself, or
/// a private member of the source that a moving method body uses.
fn conflict_warning(kind: &ParsedType, selected: &[Range<usize>]) -> Option<String> {
    let mut problems = Vec::new();
    let selected_names: Vec<&str> = kind
        .members
        .iter()
        .filter(|found| selected.contains(&found.range))
        .map(|found| found.name.as_str())
        .collect();
    for member in kind
        .members
        .iter()
        .filter(|member| selected.contains(&member.range))
    {
        if member.private {
            problems.push(format!("{} is private", member.name));
            continue;
        }
        if let Some(body) = &member.body {
            let body_text = &kind.text[body.clone()];
            for name in private_members(kind) {
                if !selected_names.contains(&name.as_str())
                    && name != member.name
                    && has_word(body_text, &name)
                {
                    problems.push(format!("{} uses the private {}", member.name, name));
                }
            }
        }
    }
    problems.sort();
    problems.dedup();
    (!problems.is_empty()).then(|| {
        format!(
            "{}; the result will not compile until these are fixed",
            problems.join(", ")
        )
    })
}

/// The warning for the members a form starts with ticked.
fn default_warning(kind: &ParsedType) -> Option<String> {
    let selected: Vec<Range<usize>> = kind
        .members
        .iter()
        .filter(|member| !member.private)
        .map(|member| member.range.clone())
        .collect();
    conflict_warning(kind, &selected)
}

/// The members that the form ticked, with the option of each.
fn selected_members<'a>(
    kind: &'a ParsedType,
    selections: &'a [MemberSelection],
) -> Result<Vec<(&'a MemberInfo, bool)>, String> {
    let mut found = Vec::new();
    for selection in selections {
        let range = parse_range(&selection.value)
            .ok_or_else(|| "The form names a member that is not there".to_owned())?;
        let member = kind
            .members
            .iter()
            .find(|member| member.range == range)
            .ok_or_else(|| "The file changed since the form was opened".to_owned())?;
        found.push((member, selection.is_abstract));
    }
    if found.is_empty() {
        return Err("No member is chosen".to_owned());
    }
    Ok(found)
}

/// Pull Members Up: the form of the members of the type around `offset` and the
/// supertypes they may go to.
pub(crate) fn pull_up_form(
    source: &SourceFile,
    offset: usize,
    targets: &[TypeFile],
) -> Option<MemberActionForm> {
    let kind = type_at(&source.text, offset)?;
    let choices = choices_of(&kind);
    let mut form = MemberActionForm {
        title: "Pull Members Up".to_owned(),
        targets: targets
            .iter()
            .map(|target| MemberTarget {
                name: target.name.clone(),
                detail: target.package.clone(),
                value: target.uri.clone(),
            })
            .collect(),
        target_label: "Supertype".to_owned(),
        targets_selectable: true,
        members: choices,
        members_label: "Members to pull up".to_owned(),
        fields: Vec::new(),
        warning: default_warning(&kind),
    };
    if form.targets.is_empty() {
        form.warning = Some("The type has no supertype file in the project".to_owned());
    }
    Some(form)
}

/// Pull Members Up: the edits of the source and the target.
pub(crate) fn pull_up_apply(
    source: &SourceFile,
    target: &SourceFile,
    selections: &[MemberSelection],
) -> Result<WorkspaceChange, String> {
    let source_kind = parsed(&source.text).ok_or_else(|| "The file declares no type".to_owned())?;
    let target_kind =
        parsed(&target.text).ok_or_else(|| "The target declares no type".to_owned())?;
    let selected = selected_members(&source_kind, selections)?;
    let mut inserted = Vec::new();
    let mut source_edits = Vec::new();
    let mut moved_text = String::new();
    for (member, is_abstract) in &selected {
        let text = if *is_abstract && member.kind == MemberKind::Method {
            abstract_signature(&member.signature, target_kind.interface)
        } else {
            member.text.clone()
        };
        if *is_abstract && member.kind == MemberKind::Method {
            // The subclass keeps its implementation.
        } else {
            source_edits.push(edit(
                &source.text,
                full_lines(&source.text, &member.range),
                "",
            ));
        }
        moved_text.push_str(&text);
        moved_text.push('\n');
        inserted.push(text);
    }
    let imports = needed_imports(&source_kind, &moved_text, &target_kind);
    let mut target_edits = Vec::new();
    // The import goes to the target file, not to its body.
    if let Some(found) = imports_edit(&target.text, &target_kind, &imports) {
        target_edits.push(found);
    }
    if let Some(body) = insert_members(&target.text, &target_kind, &inserted) {
        target_edits.push(body);
    }
    let mut edits = Vec::new();
    add_edits(&mut edits, &source.uri, source_edits);
    add_edits(&mut edits, &target.uri, target_edits);
    Ok(WorkspaceChange {
        edits,
        renames: Vec::new(),
    })
}

/// Push Members Down: the form of the members of the type around `offset`.
pub(crate) fn push_down_form(
    source: &SourceFile,
    offset: usize,
    targets: &[TypeFile],
) -> Option<MemberActionForm> {
    let kind = type_at(&source.text, offset)?;
    let choices = choices_of(&kind)
        .into_iter()
        .map(|mut member| {
            // Every subclass gets the implementation, so the target cannot stay
            // abstract on its own.
            member.can_be_abstract = false;
            member
        })
        .collect();
    Some(MemberActionForm {
        title: "Push Members Down".to_owned(),
        targets: targets
            .iter()
            .map(|target| MemberTarget {
                name: target.name.clone(),
                detail: target.package.clone(),
                value: target.uri.clone(),
            })
            .collect(),
        target_label: "Subclasses that receive the members".to_owned(),
        targets_selectable: false,
        members: choices,
        members_label: "Members to push down".to_owned(),
        fields: Vec::new(),
        warning: if targets.is_empty() {
            Some("The type has no direct subclass in the project".to_owned())
        } else {
            default_warning(&kind)
        },
    })
}

/// Push Members Down: the edits of the source and of every subclass.
pub(crate) fn push_down_apply(
    source: &SourceFile,
    targets: &[SourceFile],
    selections: &[MemberSelection],
) -> Result<WorkspaceChange, String> {
    if targets.is_empty() {
        return Err("The type has no direct subclass".to_owned());
    }
    let kind = parsed(&source.text).ok_or_else(|| "The file declares no type".to_owned())?;
    let selected = selected_members(&kind, selections)?;
    let mut source_edits = Vec::new();
    let mut moved = Vec::new();
    for (member, _) in &selected {
        source_edits.push(edit(
            &source.text,
            full_lines(&source.text, &member.range),
            "",
        ));
        moved.push(member.text.clone());
    }
    let mut edits = Vec::new();
    add_edits(&mut edits, &source.uri, source_edits);
    let moved_text = moved.join("\n");
    for target in targets {
        let Some(target_kind) = parsed(&target.text) else {
            continue;
        };
        let mut target_edits = Vec::new();
        let imports = needed_imports(&kind, &moved_text, &target_kind);
        if let Some(found) = imports_edit(&target.text, &target_kind, &imports) {
            target_edits.push(found);
        }
        if let Some(found) = insert_members(&target.text, &target_kind, &moved) {
            target_edits.push(found);
        }
        add_edits(&mut edits, &target.uri, target_edits);
    }
    Ok(WorkspaceChange {
        edits,
        renames: Vec::new(),
    })
}

/// Extract Superclass: the form of the members of the type around `offset`.
pub(crate) fn extract_superclass_form(
    source: &SourceFile,
    offset: usize,
    default_name: &str,
    packages: &[String],
) -> Option<MemberActionForm> {
    let kind = type_at(&source.text, offset)?;
    let choices = choices_of(&kind);
    let targets = packages
        .iter()
        .map(|package| MemberTarget {
            name: if package.is_empty() {
                "(default package)".to_owned()
            } else {
                package.clone()
            },
            detail: None,
            value: package.clone(),
        })
        .collect();
    Some(MemberActionForm {
        title: "Extract Superclass".to_owned(),
        targets,
        target_label: "Package".to_owned(),
        targets_selectable: true,
        members: choices,
        members_label: "Members of the new superclass".to_owned(),
        fields: vec![lang_api::ActionField {
            key: "name".to_owned(),
            label: "Superclass name".to_owned(),
            value: default_name.to_owned(),
        }],
        warning: default_warning(&kind),
    })
}

/// The text of the file a new top-level type of `package` gets.
fn new_type_text(
    package: &str,
    imports: &[String],
    declaration: &str,
    members: &[String],
) -> String {
    let mut text = String::new();
    if !package.is_empty() {
        text.push_str(&format!("package {package};\n\n"));
    }
    for line in imports {
        text.push_str(line);
        text.push('\n');
    }
    if !imports.is_empty() {
        text.push('\n');
    }
    text.push_str(declaration);
    text.push_str(" {\n");
    for (index, member) in members.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&indent_lines(member, "    "));
        text.push('\n');
    }
    text.push_str("}\n");
    text
}

/// Extract Superclass: the new file and the `extends` of the source.
pub(crate) fn extract_superclass_apply(
    source: &SourceFile,
    new_uri: &str,
    name: &str,
    package: &str,
    selections: &[MemberSelection],
) -> Result<WorkspaceChange, String> {
    let name = name.trim();
    if name.is_empty() || !is_identifier(name) {
        return Err("The superclass needs a name that Java accepts".to_owned());
    }
    let kind = parsed(&source.text).ok_or_else(|| "The file declares no type".to_owned())?;
    if name == kind.name {
        return Err("The superclass cannot have the name of the source".to_owned());
    }
    let selected = selected_members(&kind, selections)?;
    let mut members = Vec::new();
    let mut moved_text = String::new();
    let mut source_edits = Vec::new();
    for (member, is_abstract) in &selected {
        let text = if *is_abstract && member.kind == MemberKind::Method {
            abstract_signature(&member.signature, false)
        } else {
            member.text.clone()
        };
        if *is_abstract && member.kind == MemberKind::Method {
            // The source keeps the implementation.
        } else {
            source_edits.push(edit(
                &source.text,
                full_lines(&source.text, &member.range),
                "",
            ));
        }
        moved_text.push_str(&text);
        moved_text.push('\n');
        members.push(text);
    }
    let mut bare = kind.clone();
    bare.imports.clear();
    let imports = needed_imports(&kind, &moved_text, &bare);
    let header = &source.text[kind.header_range.clone()];
    let old_super = (!kind.interface)
        .then(|| header.find("extends"))
        .flatten()
        .map(|at| header[at + "extends".len()..].trim())
        .and_then(|rest| rest.split_whitespace().next())
        .map(simple_name)
        .filter(|name| !name.is_empty());
    let declaration = if kind.interface {
        format!("interface {name}")
    } else if let Some(old) = &old_super {
        format!("abstract class {name} extends {old}")
    } else {
        format!("abstract class {name}")
    };
    let new_text = new_type_text(package, &imports, &declaration, &members);

    // The source extends the new type. A class that already extends one lets the new
    // class extend that one; an interface extends the new one as well.
    let mut edits = Vec::new();
    add_edits(
        &mut edits,
        &source.uri,
        vec![extends_edit(&source.text, &kind, header, name)],
    );
    add_edits(&mut edits, &source.uri, source_edits);
    add_edits(&mut edits, new_uri, vec![edit("", 0..0, new_text)]);
    Ok(WorkspaceChange {
        edits,
        renames: Vec::new(),
    })
}

fn extends_edit(text: &str, kind: &ParsedType, header: &str, name: &str) -> DocumentEdit {
    if kind.interface {
        if let Some(at) = header.find("extends") {
            let at = kind.header_range.start + at + "extends".len();
            return edit(text, at..at, format!(" {name},"));
        }
        let at = kind.name_range.end;
        return edit(text, at..at, format!(" extends {name}"));
    }
    if let Some(at) = header.find("extends") {
        let after = kind.header_range.start + at + "extends".len();
        let rest = &text[after..kind.header_range.end];
        let old_start = after + rest.len() - rest.trim_start().len();
        let old = rest.trim_start();
        let old_end = old_start
            + old
                .find(|character: char| character.is_whitespace() || character == '{')
                .unwrap_or(old.len());
        return edit(text, old_start..old_end, name.to_owned());
    }
    let at = kind.name_range.end;
    edit(text, at..at, format!(" extends {name}"))
}

fn is_identifier(raw: &str) -> bool {
    let mut characters = raw.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '$')
        && characters
            .all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

fn capitalize(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn decapitalize(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// Introduce Parameter Object: the form of the parameters of the method around
/// `offset`.
pub(crate) fn parameter_object_form(
    source: &SourceFile,
    offset: usize,
) -> Option<MemberActionForm> {
    let method = method_at(&source.text, offset)?;
    let name = method.name.clone();
    let parameters = method.parameters.clone();
    let choices = parameters
        .iter()
        .map(|parameter| MemberChoice {
            name: parameter.name.clone(),
            detail: Some(parameter.type_name.clone()),
            checked: true,
            can_be_abstract: false,
            abstract_checked: false,
            value: range_value(&parameter.range),
        })
        .collect::<Vec<_>>();
    Some(MemberActionForm {
        title: "Introduce Parameter Object".to_owned(),
        targets: Vec::new(),
        target_label: String::new(),
        targets_selectable: false,
        members: choices,
        members_label: "Parameters to group".to_owned(),
        fields: vec![lang_api::ActionField {
            key: "name".to_owned(),
            label: "Class name".to_owned(),
            value: format!("{}Params", capitalize(&name)),
        }],
        warning: (parameters.len() < 2)
            .then(|| "A parameter object needs at least two parameters".to_owned()),
    })
}

/// Everything Introduce Parameter Object needs from the window and the project.
pub(crate) struct ParameterObjectRequest<'a> {
    pub source: &'a SourceFile,
    pub spot_offset: usize,
    pub class_name: &'a str,
    pub package: &'a str,
    pub new_uri: &'a str,
    pub use_record: bool,
    pub selections: &'a [MemberSelection],
    pub calls: &'a [CallSite],
}

/// Introduce Parameter Object: the new class, the signature, the body and every call.
pub(crate) fn parameter_object_apply(
    request: ParameterObjectRequest<'_>,
) -> Result<WorkspaceChange, String> {
    let ParameterObjectRequest {
        source,
        spot_offset,
        class_name,
        package,
        new_uri,
        use_record,
        selections,
        calls,
    } = request;
    let class_name = class_name.trim();
    if class_name.is_empty() || !is_identifier(class_name) {
        return Err("The class needs a name that Java accepts".to_owned());
    }
    let method = method_at(&source.text, spot_offset)
        .ok_or_else(|| "There is no method at the caret".to_owned())?;
    let parameters = &method.parameters;
    let chosen: Vec<&Parameter> = parameters
        .iter()
        .filter(|parameter| {
            selections.iter().any(|selection| {
                parse_range(&selection.value).is_some_and(|range| range == parameter.range)
            })
        })
        .collect();
    if chosen.is_empty() {
        return Err("No parameter is chosen".to_owned());
    }
    let selected: Vec<usize> = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| chosen.iter().any(|found| found.range == parameter.range))
        .map(|(index, _)| index)
        .collect();
    let variable = decapitalize(class_name);
    let accessor = |parameter: &Parameter| {
        if use_record {
            format!("{variable}.{}()", parameter.name)
        } else {
            format!("{variable}.get{}()", capitalize(&parameter.name))
        }
    };

    // The signature: the chosen parameters become one parameter of the new type.
    let mut kept: Vec<String> = Vec::new();
    let mut inserted = false;
    for (index, parameter) in parameters.iter().enumerate() {
        if selected.contains(&index) {
            if !inserted {
                kept.push(format!("{class_name} {variable}"));
                inserted = true;
            }
        } else {
            kept.push(parameter.raw.clone());
        }
    }
    if !inserted {
        kept.push(format!("{class_name} {variable}"));
    }
    let inside = method.parameters_range.start + 1..method.parameters_range.end.saturating_sub(1);
    let mut source_edits = vec![edit(&source.text, inside, kept.join(", "))];

    // The body: every use of a chosen parameter reads it from the object.
    if let Some(body_range) = method.body.clone() {
        let body_text = &source.text[body_range.clone()];
        let mut edits = Vec::new();
        for parameter in &chosen {
            for range in word_ranges(body_text, &parameter.name) {
                let start = body_range.start + range.start;
                let end = body_range.start + range.end;
                edits.push(edit(&source.text, start..end, accessor(parameter)));
            }
        }
        edits.sort_by_key(|found| (found.span.start.line, found.span.start.character));
        source_edits.extend(edits);
    }

    // The calls: the same arguments grouped into a new object where the first chosen
    // one stood.
    let mut by_file: Vec<(String, Vec<DocumentEdit>)> = Vec::new();
    for call in calls {
        let Some(tree) = parse(&call.text) else {
            continue;
        };
        let mut edits = Vec::new();
        for offset in &call.offsets {
            let Some((invocation, arguments)) = invocation_at(&tree, &call.text, *offset) else {
                continue;
            };
            let _ = invocation;
            let mut found = Vec::new();
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                found.push(argument_text(&call.text, argument));
            }
            if found.len() != parameters.len() {
                continue;
            }
            let mut rebuilt = Vec::new();
            let mut placed = false;
            for (index, argument) in found.iter().enumerate() {
                if selected.contains(&index) {
                    if !placed {
                        let group: Vec<String> =
                            selected.iter().map(|at| found[*at].clone()).collect();
                        rebuilt.push(format!("new {class_name}({})", group.join(", ")));
                        placed = true;
                    }
                } else {
                    rebuilt.push(argument.clone());
                }
            }
            let arguments = arguments.byte_range();
            let inside = arguments.start + 1..arguments.end.saturating_sub(1);
            edits.push(edit(&call.text, inside, rebuilt.join(", ")));
        }
        if edits.is_empty() {
            continue;
        }
        match by_file.iter_mut().find(|(uri, _)| uri == &call.uri) {
            Some((_, known)) => known.extend(edits),
            None => by_file.push((call.uri.clone(), edits)),
        }
    }

    // The new type.
    let fields: Vec<(String, String)> = chosen
        .iter()
        .map(|parameter| (parameter.type_name.clone(), parameter.name.clone()))
        .collect();
    let parts: Vec<String> = fields
        .iter()
        .map(|(type_name, name)| format!("{type_name} {name}"))
        .collect();
    let (declaration, members) = if use_record {
        (
            format!("record {class_name}({})", parts.join(", ")),
            Vec::new(),
        )
    } else {
        let mut members = Vec::new();
        for (type_name, name) in &fields {
            members.push(format!("private final {type_name} {name};"));
        }
        let mut constructor = format!("{class_name}({}) {{\n", parts.join(", "));
        for (_, name) in &fields {
            constructor.push_str(&format!("    this.{name} = {name};\n"));
        }
        constructor.push('}');
        members.push(constructor);
        for (type_name, name) in &fields {
            members.push(format!(
                "{type_name} get{}() {{\n    return {name};\n}}",
                capitalize(name)
            ));
        }
        (format!("class {class_name}"), members)
    };
    let imports = imports_for_parameters(&source.text, &chosen);
    let new_text = new_type_text(package, &imports, &declaration, &members);

    let mut edits = Vec::new();
    add_edits(&mut edits, &source.uri, source_edits);
    for (uri, file_edits) in by_file {
        add_edits(&mut edits, &uri, file_edits);
    }
    add_edits(&mut edits, new_uri, vec![edit("", 0..0, new_text)]);
    Ok(WorkspaceChange {
        edits,
        renames: Vec::new(),
    })
}

fn argument_text(text: &str, node: Node<'_>) -> String {
    text_of(text, node).trim().to_owned()
}

/// The call whose name is at `offset`, with its argument list.
fn invocation_at<'a>(tree: &'a Tree, text: &str, offset: usize) -> Option<(Node<'a>, Node<'a>)> {
    let mut node = node_at(tree, offset);
    loop {
        if node.kind() == "method_invocation" {
            let _ = text;
            return Some((node, node.child_by_field_name("arguments")?));
        }
        node = node.parent()?;
    }
}

fn imports_for_parameters(text: &str, chosen: &[&Parameter]) -> Vec<String> {
    // The new file shares the package, so only types of other packages need imports.
    // The source imports name them already; keep the ones a chosen type uses.
    let Some(kind) = parsed(text) else {
        return Vec::new();
    };
    kind.imports
        .iter()
        .filter(|line| {
            let name = line
                .trim_start_matches("import ")
                .trim_end_matches(';')
                .rsplit('.')
                .next()
                .unwrap_or_default();
            chosen
                .iter()
                .any(|parameter| has_word(&parameter.type_name, name))
        })
        .cloned()
        .collect()
}

fn word_ranges(text: &str, needle: &str) -> Vec<Range<usize>> {
    let is_word =
        |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut ranges = Vec::new();
    if needle.is_empty() {
        return ranges;
    }
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        let clean = !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word);
        if clean {
            ranges.push(start..end);
        }
        from = end;
    }
    ranges
}

/// The type names a file declares, for the hierarchy the window offers.
pub(crate) fn direct_supertypes(text: &str) -> Vec<String> {
    parsed(text).map(|kind| kind.supertypes).unwrap_or_default()
}

/// The name of the first type of a file.
pub(crate) fn type_name(text: &str) -> Option<String> {
    parsed(text).map(|kind| kind.name)
}

/// The name of the type around a byte offset.
pub(crate) fn type_name_at(text: &str, offset: usize) -> Option<String> {
    type_at(text, offset).map(|kind| kind.name)
}

/// The package of a file, for the folder of a new type.
pub(crate) fn package_name(text: &str) -> Option<String> {
    parsed(text).and_then(|kind| kind.package)
}

/// Whether a file declares a type that extends or implements `name`.
pub(crate) fn extends_or_implements(text: &str, name: &str) -> bool {
    parsed(text).is_some_and(|kind| kind.supertypes.iter().any(|found| found == name))
}

/// The files under `root` that may declare a Java type.
pub(crate) fn java_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    collect_files(root, &mut files, 0);
    files
}

fn collect_files(folder: &std::path::Path, files: &mut Vec<std::path::PathBuf>, depth: usize) {
    if depth > 24 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIPPED_FOLDERS.contains(&name.as_ref()) {
                continue;
            }
            collect_files(&path, files, depth + 1);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
        {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use lang_api::MemberSelection;

    use super::{
        SourceFile, extract_superclass_apply, parameter_object_apply, pull_up_apply,
        push_down_apply,
    };

    fn source(uri: &str, text: &str) -> SourceFile {
        SourceFile {
            uri: uri.to_owned(),
            text: text.to_owned(),
        }
    }

    fn selection(text: &str, name: &str, is_abstract: bool) -> MemberSelection {
        let kind = super::parsed(text).unwrap();
        let member = kind
            .members
            .iter()
            .find(|member| member.name == name)
            .unwrap();
        MemberSelection {
            value: format!("{}:{}", member.range.start, member.range.end),
            is_abstract,
        }
    }

    fn text_of(change: &lang_api::WorkspaceChange, uri: &str, before: &str) -> String {
        let edits: Vec<lang_api::DocumentEdit> = change
            .edits
            .iter()
            .filter(|source| source.uri == uri)
            .flat_map(|source| source.edits.clone())
            .collect();
        lang_api::apply_to_text(before, &edits)
    }

    #[test]
    fn pull_up_moves_a_method_and_a_field_into_the_superclass() {
        let base = source("file:///a/Base.java", "package a;\n\nclass Base {\n}\n");
        let child_text = "package a;\n\nclass Child extends Base {\n    private int count = 1;\n\n    public int value() {\n        return count;\n    }\n}\n";
        let child = source("file:///a/Child.java", child_text);
        let change = pull_up_apply(
            &child,
            &base,
            &[
                selection(child_text, "value", false),
                selection(child_text, "count", false),
            ],
        )
        .unwrap();
        assert_eq!(
            text_of(&change, &base.uri, &base.text),
            "package a;\n\nclass Base {\n    public int value() {\n        return count;\n    }\n\n    private int count = 1;\n}\n"
        );
        assert_eq!(
            text_of(&change, &child.uri, child_text),
            "package a;\n\nclass Child extends Base {\n}\n"
        );
    }

    #[test]
    fn push_down_moves_a_method_into_the_two_subclasses() {
        let parent_text =
            "package a;\n\nclass Parent {\n    public int value() {\n        return 1;\n    }\n}\n";
        let parent = source("file:///a/Parent.java", parent_text);
        let first = source(
            "file:///a/First.java",
            "package a;\n\nclass First extends Parent {\n}\n",
        );
        let second = source(
            "file:///a/Second.java",
            "package a;\n\nclass Second extends Parent {\n}\n",
        );
        let change = push_down_apply(
            &parent,
            &[first.clone(), second.clone()],
            &[selection(parent_text, "value", false)],
        )
        .unwrap();
        assert_eq!(
            text_of(&change, &parent.uri, parent_text),
            "package a;\n\nclass Parent {\n}\n"
        );
        for child in [first, second] {
            assert_eq!(
                text_of(&change, &child.uri, &child.text),
                "package a;\n\nclass First extends Parent {\n    public int value() {\n        return 1;\n    }\n}\n"
                    .replace("First", if child.uri.contains("First") { "First" } else { "Second" })
            );
        }
    }

    #[test]
    fn extract_superclass_creates_the_file_and_extends_it() {
        let child_text = "package a;\n\nclass Child {\n    public int first() {\n        return 1;\n    }\n\n    public int second() {\n        return 2;\n    }\n}\n";
        let child = source("file:///a/Child.java", child_text);
        let change = extract_superclass_apply(
            &child,
            "file:///a/AbstractChild.java",
            "AbstractChild",
            "a",
            &[
                selection(child_text, "first", false),
                selection(child_text, "second", false),
            ],
        )
        .unwrap();
        assert_eq!(
            text_of(&change, &child.uri, child_text),
            "package a;\n\nclass Child extends AbstractChild {\n}\n"
        );
        assert_eq!(
            text_of(&change, "file:///a/AbstractChild.java", ""),
            "package a;\n\nabstract class AbstractChild {\n    public int first() {\n        return 1;\n    }\n\n    public int second() {\n        return 2;\n    }\n}\n"
        );
    }

    #[test]
    fn a_parameter_object_takes_two_parameters_and_its_call() {
        let user_text = "package a;\n\nclass User {\n    void move(int x, int y) {\n        System.out.println(x + y);\n    }\n\n    void run() {\n        move(1, 2);\n    }\n}\n";
        let user = source("file:///a/User.java", user_text);
        let spot = user_text.find("void move").unwrap();
        let kind = super::parsed(user_text).unwrap();
        let method = kind
            .members
            .iter()
            .find(|member| member.name == "move")
            .unwrap();
        let parameters = super::method_at(user_text, method.range.start + 1).unwrap();
        let call = user_text.rfind("move(1, 2)").unwrap();
        let calls = [super::CallSite {
            uri: user.uri.clone(),
            text: user_text.to_owned(),
            offsets: vec![call],
        }];
        let change = parameter_object_apply(super::ParameterObjectRequest {
            source: &user,
            spot_offset: spot,
            class_name: "Point",
            package: "a",
            new_uri: "file:///a/Point.java",
            use_record: true,
            selections: &[
                MemberSelection {
                    value: format!(
                        "{}:{}",
                        parameters.parameters[0].range.start, parameters.parameters[0].range.end
                    ),
                    is_abstract: false,
                },
                MemberSelection {
                    value: format!(
                        "{}:{}",
                        parameters.parameters[1].range.start, parameters.parameters[1].range.end
                    ),
                    is_abstract: false,
                },
            ],
            calls: &calls,
        })
        .unwrap();
        assert_eq!(
            text_of(&change, &user.uri, user_text),
            "package a;\n\nclass User {\n    void move(Point point) {\n        System.out.println(point.x() + point.y());\n    }\n\n    void run() {\n        move(new Point(1, 2));\n    }\n}\n"
        );
        assert_eq!(
            text_of(&change, "file:///a/Point.java", ""),
            "package a;\n\nrecord Point(int x, int y) {\n}\n"
        );
    }
}

#[cfg(test)]
mod move_tests {
    use lang_api::MovePlace;

    use super::{move_members, move_place};

    const CLASS: &str = "package a;\n\nclass Owner {\n    static void save() {\n    }\n\n    String name() {\n        return \"\";\n    }\n}\n";

    #[test]
    fn a_static_member_moves_and_an_instance_member_says_why_not() {
        let save = CLASS.find("save").unwrap();
        assert_eq!(move_place(CLASS, save), MovePlace::Member);
        let name = CLASS.find("name").unwrap();
        assert!(matches!(move_place(CLASS, name), MovePlace::Unmovable(_)));
        let file = CLASS.find("Owner").unwrap();
        assert_eq!(move_place(CLASS, file), MovePlace::File);
    }

    #[test]
    fn the_form_lists_the_static_members_and_ticks_the_one_at_the_caret() {
        let save = CLASS.find("save").unwrap();
        let members = move_members(CLASS, save).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "save");
        assert!(members[0].checked);
        assert_eq!(members[0].span.start.line, 3);
    }

    #[test]
    fn a_field_of_an_interface_is_a_constant_without_the_modifier() {
        let text = "package a;\n\ninterface Values {\n    int MAX = 10;\n}\n";
        let max = text.find("MAX").unwrap();
        assert_eq!(move_place(text, max), MovePlace::Member);
        assert_eq!(move_members(text, max).unwrap()[0].name, "MAX");
    }
}
