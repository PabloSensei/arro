//! Structural search and replace: a template of code with `$name$` variables, matched
//! against a tree. Whitespace and comments do not count, the order of modifiers does not
//! matter, and a variable can stand for a sequence of nodes when its count allows it.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use lang_api::{
    StructuralEdit, StructuralMatch, StructuralQuery, StructuralTemplate, StructuralVariable,
};
use regex::Regex;
use tree_sitter::{Node, Parser, Tree};

type Bindings = BTreeMap<String, Bound>;

#[derive(Clone)]
struct Bound {
    /// The bytes of the nodes the variable matched, with the separators between them.
    range: Range<usize>,
    /// How many nodes the variable matched.
    count: usize,
    /// The text with its whitespace runs collapsed, for the repeat of a variable.
    text: String,
}

enum Wrapper {
    Expression,
    Statement,
    Member,
    Type,
}

/// A template with its placeholders read out of the tree.
struct ParsedTemplate {
    /// The template with every `$name$` replaced by an identifier, so that the grammar
    /// reads it.
    source: String,
    tree: Tree,
    /// The child indices from the root of `tree` to the node that is matched.
    path: Vec<u32>,
    kind: String,
    /// Placeholder identifier -> variable name.
    placeholders: HashMap<String, String>,
    variables: Vec<String>,
    /// The variables that stand for a whole parameter list, which the grammar reads as
    /// a dummy parameter.
    parameter_variables: std::collections::HashSet<String>,
    /// How many nodes each variable may take; one when the window sets no count.
    counts: BTreeMap<String, (usize, Option<usize>)>,
}

/// A compiled search: the template, the filters of its variables and the replacement.
pub struct Search {
    template: ParsedTemplate,
    filters: BTreeMap<String, StructuralVariable>,
    replacement: Option<String>,
}

impl Search {
    /// Reads a template. The pattern is tried as an expression, a statement, a member
    /// and a type, in that order, as IDEA does.
    pub fn new(query: &StructuralQuery) -> Result<Self, String> {
        let replacement = query.replacement.as_ref().filter(|it| !it.is_empty());
        let template = ParsedTemplate::parse(&query.pattern)?;
        let filters = query
            .variables
            .iter()
            .filter(|variable| template.variables.contains(&variable.name))
            .map(|variable| (variable.name.clone(), variable.clone()))
            .collect();
        let mut search = Self {
            template,
            filters,
            replacement: replacement.cloned(),
        };
        for name in &search.template.variables {
            let bounds = search
                .filters
                .get(name)
                .and_then(|filter| filter.count)
                .unwrap_or((1, Some(1)));
            search.template.counts.insert(name.clone(), bounds);
        }
        if let Some(replacement) = &search.replacement {
            search.template.check_replacement(replacement)?;
        }
        search.check_filters()?;
        Ok(search)
    }

    fn check_filters(&self) -> Result<(), String> {
        for (name, filter) in &self.filters {
            if let Some(text) = &filter.text {
                let pattern = text.strip_prefix('!').unwrap_or(text);
                Regex::new(pattern).map_err(|error| format!("the text of ${name}$: {error}"))?;
            }
            if let Some(pattern) = &filter.type_pattern {
                Regex::new(pattern).map_err(|error| format!("the type of ${name}$: {error}"))?;
            }
        }
        Ok(())
    }

    fn bounds_of(&self, name: &str) -> (usize, Option<usize>) {
        self.template.bounds_of(name)
    }
    /// Every place of `text` the template matches, outermost first.
    pub fn find(&self, text: &str) -> Vec<StructuralMatch> {
        let Some(tree) = parse(text) else {
            return Vec::new();
        };
        let declarations = Declarations::read(tree.root_node(), text);
        let mut found = Vec::new();
        let mut accepted: Vec<Range<usize>> = Vec::new();
        let mut pending = vec![tree.root_node()];
        // A pre-order walk keeps an outer match before the matches inside it.
        while let Some(node) = pending.pop() {
            if same_kind(&self.template.kind, node.kind())
                && !accepted
                    .iter()
                    .any(|kept| kept.start <= node.start_byte() && node.end_byte() <= kept.end)
            {
                let mut bindings = Bindings::new();
                if self.template.matches(node, text, &mut bindings)
                    && let Some(found_match) = self.finish(node, text, &bindings, &declarations)
                {
                    accepted.push(found_match.start..found_match.end);
                    found.push(found_match);
                }
            }
            let mut cursor = node.walk();
            let mut children: Vec<Node> = node.children(&mut cursor).collect();
            children.reverse();
            pending.extend(children);
        }
        found.sort_by_key(|found| found.start);
        found
    }

    /// The edits of Replace Structurally, sorted and without overlaps.
    pub fn replace(&self, text: &str) -> Result<Vec<StructuralEdit>, String> {
        if self.replacement.is_none() {
            return Ok(Vec::new());
        }
        let mut edits = Vec::new();
        for found in self.find(text) {
            let Some(replacement) = found.replacement else {
                continue;
            };
            edits.push(StructuralEdit {
                start: found.start,
                end: found.end,
                replacement,
            });
        }
        Ok(edits)
    }

    fn finish(
        &self,
        node: Node<'_>,
        text: &str,
        bindings: &Bindings,
        declarations: &Declarations,
    ) -> Option<StructuralMatch> {
        let mut notes = Vec::new();
        for (name, filter) in &self.filters {
            let Some(bound) = bindings.get(name) else {
                continue;
            };
            if !self.passes(name, filter, bound, text, node, declarations, &mut notes) {
                return None;
            }
        }
        let start = node.start_byte();
        let end = node.end_byte();
        let replacement = self
            .replacement
            .as_ref()
            .map(|replacement| self.render(replacement, text, bindings, start));
        Some(StructuralMatch {
            start,
            end,
            text: text[start..end].to_owned(),
            replacement,
            notes,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn passes(
        &self,
        name: &str,
        filter: &StructuralVariable,
        bound: &Bound,
        text: &str,
        node: Node<'_>,
        declarations: &Declarations,
        notes: &mut Vec<String>,
    ) -> bool {
        let (min, max) = self.bounds_of(name);
        if bound.count < min || max.is_some_and(|max| bound.count > max) {
            return false;
        }
        if let Some(filter_text) = &filter.text {
            let (inverted, pattern) = match filter_text.strip_prefix('!') {
                Some(pattern) => (true, pattern),
                None => (false, filter_text.as_str()),
            };
            let Ok(regex) = Regex::new(pattern) else {
                return false;
            };
            let matched = regex.is_match(&bound.text);
            if matched == inverted {
                return false;
            }
        }
        if let Some(pattern) = &filter.type_pattern {
            let Ok(regex) = Regex::new(pattern) else {
                return false;
            };
            match declarations.type_of(node, bound, text) {
                Some(found) => {
                    let simple = simple_type_name(&found);
                    if !regex.is_match(&simple) {
                        return false;
                    }
                }
                None => notes.push(format!("type not checked: ${name}$")),
            }
        }
        true
    }

    /// The template of the replacement with the text of the variables, moved to the
    /// indentation of the match.
    fn render(&self, replacement: &str, text: &str, bindings: &Bindings, start: usize) -> String {
        let mut rendered = String::new();
        let mut rest = replacement;
        while let Some(at) = rest.find('$') {
            rendered.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let Some(end) = after.find('$') else {
                rendered.push_str(&rest[at..]);
                rest = "";
                break;
            };
            let name = &after[..end];
            match bindings.get(name) {
                Some(bound) => rendered.push_str(&text[bound.range.clone()]),
                None => {
                    rendered.push('$');
                    rendered.push_str(name);
                    rendered.push('$');
                }
            }
            rest = &after[end + 1..];
        }
        rendered.push_str(rest);
        indent_replacement(&rendered, text, start)
    }
}

/// The lines after the first one take the indentation of the line the match starts on.
/// A replacement that starts the line carries no indentation of its own, because the
/// text before the match keeps it.
fn indent_replacement(rendered: &str, text: &str, start: usize) -> String {
    if !rendered.contains('\n') {
        return rendered.to_owned();
    }
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let before = &text[line_start..start];
    let indent: String = before
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .collect();
    let at_line_start = before.trim().is_empty();
    let mut lines: Vec<String> = rendered.split('\n').map(str::to_owned).collect();
    if at_line_start
        && let Some(first) = lines.first_mut()
        && let Some(rest) = first.strip_prefix(&indent)
    {
        *first = rest.to_owned();
    }
    for line in lines.iter_mut().skip(1) {
        if line.trim().is_empty() {
            line.clear();
            continue;
        }
        let mut moved = indent.clone();
        moved.push_str(line);
        *line = moved;
    }
    lines.join("\n")
}

impl ParsedTemplate {
    fn bounds_of(&self, name: &str) -> (usize, Option<usize>) {
        self.counts.get(name).copied().unwrap_or((1, Some(1)))
    }

    fn parse(pattern: &str) -> Result<Self, String> {
        // A variable alone between parentheses is an argument list in most templates,
        // and a parameter list in a method. The grammar reads a lone name there as a
        // parameter without a type, so that reading is tried when the plain one fails.
        if let Some(template) = Self::parse_mode(pattern, false) {
            return Ok(template);
        }
        if let Some(template) = Self::parse_mode(pattern, true) {
            return Ok(template);
        }
        Err("the template is not an expression, a statement, a member or a type".to_owned())
    }

    fn parse_mode(pattern: &str, parameter_mode: bool) -> Option<Self> {
        let (source, placeholders, parameter_variables) = substitute(pattern, parameter_mode);
        for wrapper in [
            Wrapper::Expression,
            Wrapper::Statement,
            Wrapper::Member,
            Wrapper::Type,
        ] {
            let wrapped = wrap(&source, &wrapper);
            let Some(tree) = parse(&wrapped) else {
                continue;
            };
            if !clean(tree.root_node()) {
                continue;
            }
            let Some((node, path)) = locate(&tree, &wrapper) else {
                continue;
            };
            let mut variables: Vec<String> = Vec::new();
            collect_variables(node, &wrapped, &placeholders, &mut variables);
            return Some(Self {
                source: wrapped,
                kind: node.kind().to_owned(),
                tree,
                path,
                placeholders,
                variables,
                parameter_variables,
                counts: BTreeMap::new(),
            });
        }
        None
    }

    fn root(&self) -> Node<'_> {
        let mut node = self.tree.root_node();
        for index in &self.path {
            node = node.child(*index).unwrap_or(node);
        }
        node
    }

    fn placeholder(&self, node: Node<'_>) -> Option<&str> {
        let text = self.source.get(node.byte_range())?;
        self.placeholders.get(text).map(String::as_str)
    }

    fn matches(&self, target: Node<'_>, text: &str, bindings: &mut Bindings) -> bool {
        let root = self.root();
        self.match_node(root, target, text, bindings)
    }

    fn match_node(
        &self,
        pattern: Node<'_>,
        target: Node<'_>,
        text: &str,
        bindings: &mut Bindings,
    ) -> bool {
        if let Some(name) = self.placeholder(pattern) {
            return bind(name, &[target], text, bindings);
        }
        // `@$annotation$` stands for the whole annotation, with its arguments.
        if matches!(pattern.kind(), "marker_annotation" | "annotation")
            && matches!(target.kind(), "marker_annotation" | "annotation")
            && let Some(name_node) = pattern.child_by_field_name("name")
            && let Some(name) = self.placeholder(name_node)
        {
            return bind(name, &[target], text, bindings);
        }
        if !same_kind(pattern.kind(), target.kind()) {
            return false;
        }
        if pattern.child_count() == 0 {
            return self.source[pattern.byte_range()] == text[target.byte_range()];
        }
        if pattern.kind() == "modifiers" {
            return self.match_modifiers(pattern, target, text, bindings);
        }
        if is_list(pattern.kind()) {
            return self.match_list(pattern, target, text, bindings);
        }
        self.match_children(pattern, target, text, bindings)
    }

    fn match_children(
        &self,
        pattern: Node<'_>,
        target: Node<'_>,
        text: &str,
        bindings: &mut Bindings,
    ) -> bool {
        let mut pattern_children = children(pattern);
        let mut target_children = children(target);
        // A template without modifiers accepts any modifiers of the match.
        if !pattern_children
            .iter()
            .any(|child| child.kind() == "modifiers")
        {
            target_children.retain(|child| child.kind() != "modifiers");
        }
        // A declaration without an initializer matches one that has it.
        if pattern.kind() == "variable_declarator"
            && !pattern_children.iter().any(|child| child.kind() == "=")
            && let Some(at) = target_children.iter().position(|child| child.kind() == "=")
        {
            target_children.truncate(at);
        }
        if pattern_children.len() != target_children.len() {
            return false;
        }
        for (pattern_child, target_child) in pattern_children.drain(..).zip(target_children) {
            if !self.match_node(pattern_child, target_child, text, bindings) {
                return false;
            }
        }
        true
    }

    /// Modifiers match as a set: their order does not matter, and the template names the
    /// ones it requires while the match may have more.
    fn match_modifiers(
        &self,
        pattern: Node<'_>,
        target: Node<'_>,
        text: &str,
        bindings: &mut Bindings,
    ) -> bool {
        let pattern_children = children(pattern);
        let mut target_children = children(target);
        for pattern_child in pattern_children {
            let mut matched = None;
            for (index, target_child) in target_children.iter().enumerate() {
                let mut attempt = bindings.clone();
                if self.match_node(pattern_child, *target_child, text, &mut attempt) {
                    matched = Some(index);
                    *bindings = attempt;
                    break;
                }
            }
            let Some(index) = matched else {
                return false;
            };
            target_children.remove(index);
        }
        true
    }

    /// A list of nodes: arguments, statements, members. A variable may take a run of
    /// them when its count allows it.
    fn match_list(
        &self,
        pattern: Node<'_>,
        target: Node<'_>,
        text: &str,
        bindings: &mut Bindings,
    ) -> bool {
        let pattern_children = named_children(pattern);
        let mut pattern_items = Vec::new();
        for child in pattern_children {
            if let Some(name) = self.placeholder(child) {
                pattern_items.push(PatternItem::Variable(name.to_owned()));
                continue;
            }
            if child.kind() == "expression_statement" {
                let named = named_children(child);
                if let [only] = named.as_slice()
                    && let Some(name) = self.placeholder(*only)
                {
                    pattern_items.push(PatternItem::Variable(name.to_owned()));
                    continue;
                }
            }
            if child.kind() == "formal_parameter"
                && let Some(name_node) = child.child_by_field_name("name")
                && let Some(name) = self.placeholder(name_node)
                && self.parameter_variables.contains(name)
            {
                pattern_items.push(PatternItem::Variable(name.to_owned()));
                continue;
            }
            pattern_items.push(PatternItem::Node(child));
        }
        let target_items = named_children(target);
        self.match_items(&pattern_items, &target_items, text, bindings)
    }

    fn match_items(
        &self,
        pattern: &[PatternItem<'_>],
        target: &[Node<'_>],
        text: &str,
        bindings: &mut Bindings,
    ) -> bool {
        let Some(first) = pattern.first() else {
            return target.is_empty();
        };
        let rest = &pattern[1..];
        match first {
            PatternItem::Node(node) => {
                let Some(target_node) = target.first() else {
                    return false;
                };
                let mut attempt = bindings.clone();
                if self.match_node(*node, *target_node, text, &mut attempt)
                    && self.match_items(rest, &target[1..], text, &mut attempt)
                {
                    *bindings = attempt;
                    return true;
                }
                false
            }
            PatternItem::Variable(name) => {
                let (min, max) = self.bounds_of(name);
                let available = target.len();
                let max = max.map_or(available, |max| max.min(available));
                if min > max {
                    return false;
                }
                for count in (min..=max).rev() {
                    let mut attempt = bindings.clone();
                    if bind(name, &target[..count], text, &mut attempt)
                        && self.match_items(rest, &target[count..], text, &mut attempt)
                    {
                        *bindings = attempt;
                        return true;
                    }
                }
                false
            }
        }
    }

    fn check_replacement(&self, replacement: &str) -> Result<(), String> {
        for name in variable_names(replacement) {
            if !self.variables.contains(&name) {
                return Err(format!("${name}$ is not in the template"));
            }
        }
        Ok(())
    }
}

enum PatternItem<'a> {
    Node(Node<'a>),
    Variable(String),
}

fn bind(name: &str, nodes: &[Node<'_>], text: &str, bindings: &mut Bindings) -> bool {
    let (Some(first), Some(last)) = (nodes.first(), nodes.last()) else {
        let bound = Bound {
            range: 0..0,
            count: 0,
            text: String::new(),
        };
        return match bindings.get(name) {
            Some(old) => old.text == bound.text && old.count == bound.count,
            None => {
                bindings.insert(name.to_owned(), bound);
                true
            }
        };
    };
    let bound = Bound {
        range: first.start_byte()..last.end_byte(),
        count: nodes.len(),
        text: normalize(&text[first.start_byte()..last.end_byte()]),
    };
    match bindings.get(name) {
        Some(old) => old.text == bound.text && old.count == bound.count,
        None => {
            bindings.insert(name.to_owned(), bound);
            true
        }
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// All children but the comments, whose places do not matter.
fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| !is_comment(*child))
        .collect()
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !is_comment(*child))
        .collect()
}

fn is_comment(node: Node<'_>) -> bool {
    node.kind() == "line_comment" || node.kind() == "block_comment"
}

/// A declaration of a variable is the same shape inside a method and at a field, and a
/// template without a wrapper is read as the first. Both kinds match each other.
fn same_kind(pattern: &str, target: &str) -> bool {
    if pattern == target {
        return true;
    }
    let declarations = [
        "local_variable_declaration",
        "field_declaration",
        "constant_declaration",
    ];
    declarations.contains(&pattern) && declarations.contains(&target)
}

fn is_list(kind: &str) -> bool {
    matches!(
        kind,
        "argument_list"
            | "block"
            | "class_body"
            | "constructor_body"
            | "interface_body"
            | "enum_body"
            | "array_initializer"
            | "formal_parameters"
            | "type_parameters"
            | "switch_block_statement_group"
            | "module_body"
    )
}

fn clean(node: Node<'_>) -> bool {
    if node.is_error() || node.is_missing() {
        return false;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).all(clean)
}

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&super::config().grammar).ok()?;
    parser.parse(text, None)
}

fn wrap(source: &str, wrapper: &Wrapper) -> String {
    match wrapper {
        Wrapper::Expression => format!("class Wrapper {{ void method() {{ {source}; }} }}"),
        Wrapper::Statement => format!("class Wrapper {{ void method() {{ {source} }} }}"),
        Wrapper::Member => format!("class Wrapper {{ {source} }}"),
        Wrapper::Type => source.to_owned(),
    }
}

/// The node of the template inside its wrapper, with the child indices from the root.
fn locate<'a>(tree: &'a Tree, wrapper: &Wrapper) -> Option<(Node<'a>, Vec<u32>)> {
    let root = tree.root_node();
    match wrapper {
        Wrapper::Expression | Wrapper::Statement => {
            let class = root.named_child(0)?;
            let kind = class.kind();
            if !matches!(kind, "class_declaration" | "interface_declaration") {
                return None;
            }
            let body = class.child_by_field_name("body")?;
            let method = named_children(body).into_iter().next()?;
            let block = method.child_by_field_name("body")?;
            let first = named_children(block).into_iter().next()?;
            if named_children(block).len() != 1 {
                return None;
            }
            if matches!(wrapper, Wrapper::Expression) {
                if first.kind() != "expression_statement" {
                    return None;
                }
                let expression = named_children(first).into_iter().next()?;
                if named_children(first).len() != 1 {
                    return None;
                }
                let path = path_for(root, expression)?;
                Some((expression, path))
            } else {
                let path = path_for(root, first)?;
                Some((first, path))
            }
        }
        Wrapper::Member => {
            let class = root.named_child(0)?;
            let body = class.child_by_field_name("body")?;
            let members = named_children(body);
            if members.len() != 1 {
                return None;
            }
            let path = path_for(root, members[0])?;
            Some((members[0], path))
        }
        Wrapper::Type => {
            let types = named_children(root);
            if types.len() != 1 || !is_type(types[0].kind()) {
                return None;
            }
            let path = path_for(root, types[0])?;
            Some((types[0], path))
        }
    }
}

fn is_type(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

/// The child indices from `root` to `node`, `None` when `node` is not below `root`.
fn path_for(root: Node<'_>, node: Node<'_>) -> Option<Vec<u32>> {
    let mut path = Vec::new();
    let mut current = node;
    while current.id() != root.id() {
        let parent = current.parent()?;
        let mut cursor = parent.walk();
        let index = parent
            .children(&mut cursor)
            .position(|child| child.id() == current.id())?;
        path.push(index as u32);
        current = parent;
    }
    path.reverse();
    Some(path)
}

/// Replaces `$name$` with an identifier and remembers which one belongs to which name.
/// In parameter mode a variable alone between parentheses becomes a whole parameter, so
/// that a template like `f($parameters$)` reads as a signature.
fn substitute(
    pattern: &str,
    parameter_mode: bool,
) -> (
    String,
    HashMap<String, String>,
    std::collections::HashSet<String>,
) {
    let mut source = String::new();
    let mut placeholders = HashMap::new();
    let mut parameters = std::collections::HashSet::new();
    let mut rest = pattern;
    let mut consumed = 0;
    while let Some(at) = rest.find('$') {
        source.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('$') else {
            source.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        if !is_variable_name(name) {
            source.push('$');
            source.push_str(name);
            source.push('$');
            rest = &after[end + 1..];
            consumed = pattern.len() - rest.len();
            continue;
        }
        let token = format!("__structural_{}", placeholders.len());
        placeholders.insert(token.clone(), name.to_owned());
        let before = &pattern[..consumed + at];
        let closing = consumed + at + 1 + end + 1;
        let after_variable = pattern.get(closing..).unwrap_or_default();
        if parameter_mode
            && before.trim_end().ends_with('(')
            && after_variable.trim_start().starts_with(')')
        {
            source.push_str("Object ");
            parameters.insert(name.to_owned());
        }
        source.push_str(&token);
        rest = &after[end + 1..];
        consumed = pattern.len() - rest.len();
    }
    source.push_str(rest);
    (source, placeholders, parameters)
}

fn is_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn variable_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        let after = &rest[at + 1..];
        let Some(end) = after.find('$') else {
            break;
        };
        let name = &after[..end];
        if is_variable_name(name) && !names.iter().any(|kept| kept == name) {
            names.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    names
}

fn collect_variables(
    node: Node<'_>,
    source: &str,
    placeholders: &HashMap<String, String>,
    variables: &mut Vec<String>,
) {
    let text = source.get(node.byte_range()).unwrap_or_default();
    if let Some(name) = placeholders.get(text)
        && !variables.contains(name)
    {
        variables.push(name.clone());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_variables(child, source, placeholders, variables);
    }
}

/// The variables of a template, in the order they first appear. The window lists them
/// with their filters.
pub fn variables(pattern: &str) -> Result<Vec<String>, String> {
    Ok(ParsedTemplate::parse(pattern)?.variables)
}

pub fn find(query: &StructuralQuery, text: &str) -> Result<Vec<StructuralMatch>, String> {
    Ok(Search::new(query)?.find(text))
}

pub fn replace(query: &StructuralQuery, text: &str) -> Result<Vec<StructuralEdit>, String> {
    Search::new(query)?.replace(text)
}

/// The type of a variable of a template, read from the declarations of the same file.
/// IDEA asks its resolve engine; here the declaration next to the place is enough, and
/// a place without one passes the filter with a note.
struct Declarations {
    /// Name, the type text and the scope of the declaration.
    entries: Vec<Declaration>,
}

struct Declaration {
    name: String,
    type_text: String,
    scope: Range<usize>,
    start: usize,
}

impl Declarations {
    fn read(root: Node<'_>, text: &str) -> Self {
        let mut entries = Vec::new();
        collect_declarations(root, text, &mut entries);
        Self { entries }
    }

    fn type_of(&self, node: Node<'_>, bound: &Bound, text: &str) -> Option<String> {
        let end = bound.range.end.saturating_sub(1).max(bound.range.start);
        let binding = node.descendant_for_byte_range(bound.range.start, end)?;
        let kind = binding.kind();
        // The variable is a type of the template, as in `for ($T$ $v$ : $c$)`.
        if is_type_node(kind) {
            return Some(text[binding.byte_range()].to_owned());
        }
        // The variable is the name of a declaration, whose type is on its left.
        let name = match kind {
            "identifier" => text[binding.byte_range()].to_owned(),
            "variable_declarator"
            | "formal_parameter"
            | "spread_parameter"
            | "catch_formal_parameter" => {
                let name = binding.child_by_field_name("name")?;
                text[name.byte_range()].to_owned()
            }
            _ => return None,
        };
        let mut best: Option<&Declaration> = None;
        for entry in &self.entries {
            if entry.name != name
                || entry.scope.start > node.start_byte()
                || node.end_byte() > entry.scope.end
                || entry.start > node.start_byte()
            {
                continue;
            }
            if best.is_none_or(|best| entry.scope.start >= best.scope.start) {
                best = Some(entry);
            }
        }
        best.map(|entry| entry.type_text.clone())
    }
}

fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "array_type"
            | "integral_type"
            | "floating_point_type"
            | "boolean_type"
            | "void_type"
            | "int"
            | "long"
            | "short"
            | "byte"
            | "char"
            | "float"
            | "double"
            | "boolean"
            | "void"
    )
}

fn collect_declarations(node: Node<'_>, text: &str, entries: &mut Vec<Declaration>) {
    let type_of = |node: Node<'_>| {
        node.child_by_field_name("type")
            .map(|node| text[node.byte_range()].to_owned())
    };
    let scope = |node: Node<'_>| {
        let mut current = node.parent();
        while let Some(found) = current {
            if matches!(
                found.kind(),
                "block" | "class_body" | "constructor_body" | "interface_body"
            ) {
                return found.byte_range();
            }
            current = found.parent();
        }
        0..text.len()
    };
    match node.kind() {
        "local_variable_declaration"
        | "field_declaration"
        | "constant_declaration"
        | "resource" => {
            if let Some(type_text) = type_of(node) {
                let scope = scope(node);
                for declarator in named_children(node) {
                    if declarator.kind() != "variable_declarator" {
                        continue;
                    }
                    if let Some(name) = declarator.child_by_field_name("name") {
                        let name = text[name.byte_range()].to_owned();
                        entries.push(Declaration {
                            name,
                            type_text: type_text.clone(),
                            scope: scope.clone(),
                            start: node.start_byte(),
                        });
                    }
                }
            }
        }
        "formal_parameter" | "spread_parameter" | "catch_formal_parameter" => {
            if let Some(type_text) = type_of(node)
                && let Some(name) = node.child_by_field_name("name")
            {
                let name = text[name.byte_range()].to_owned();
                entries.push(Declaration {
                    name,
                    type_text,
                    scope: scope(node),
                    start: node.start_byte(),
                });
            }
        }
        "enhanced_for_statement" => {
            if let Some(type_text) = type_of(node) {
                let scope = scope(node);
                for child in named_children(node) {
                    let name = match child.kind() {
                        "variable_declarator" => child.child_by_field_name("name"),
                        _ => None,
                    };
                    if let Some(name) = name {
                        let name = text[name.byte_range()].to_owned();
                        entries.push(Declaration {
                            name,
                            type_text: type_text.clone(),
                            scope: scope.clone(),
                            start: node.start_byte(),
                        });
                    }
                }
                if let Some(name) = node.child_by_field_name("name") {
                    let name = text[name.byte_range()].to_owned();
                    entries.push(Declaration {
                        name,
                        type_text,
                        scope,
                        start: node.start_byte(),
                    });
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_declarations(child, text, entries);
    }
}

/// The simple name of a type: `java.util.List<Owner>` and `Owner[]` give `List` and
/// `Owner`.
fn simple_type_name(text: &str) -> String {
    let mut name = text.trim();
    if let Some(at) = name.find('<') {
        name = &name[..at];
    }
    let name = name.trim_end_matches("[]").trim();
    let name = name.rsplit('.').next().unwrap_or(name);
    name.trim().to_owned()
}

fn variable(name: &str, count: Option<(usize, Option<usize>)>) -> StructuralVariable {
    StructuralVariable {
        name: name.to_owned(),
        count,
        ..StructuralVariable::default()
    }
}

fn template(
    id: &str,
    name: &str,
    description: &str,
    pattern: &str,
    replacement: Option<&str>,
    variables: Vec<StructuralVariable>,
) -> StructuralTemplate {
    StructuralTemplate {
        id: id.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        pattern: pattern.to_owned(),
        replacement: replacement.map(str::to_owned),
        variables,
    }
}

/// The templates ready to use, named the way the IDE names them.
pub fn ready_templates() -> Vec<StructuralTemplate> {
    vec![
        template(
            "method_calls",
            "Method calls",
            "A call of any method on any receiver",
            "$object$.$method$($arguments$)",
            None,
            vec![
                variable("object", None),
                variable("method", None),
                variable("arguments", Some((0, None))),
            ],
        ),
        template(
            "if_statements",
            "If statements",
            "An if with a block",
            "if ($condition$) { $statements$; }",
            None,
            vec![
                variable("condition", None),
                variable("statements", Some((1, None))),
            ],
        ),
        template(
            "try_without_finally",
            "Try without finally",
            "A try with a catch and no finally",
            "try { $body$; } catch ($type$ $exception$) { $handler$; }",
            None,
            vec![
                variable("body", Some((1, None))),
                variable("type", None),
                variable("exception", None),
                variable("handler", Some((0, None))),
            ],
        ),
        template(
            "equals_with_null",
            "Equals with null",
            "A call of equals on null",
            "$object$.equals(null)",
            None,
            vec![variable("object", None)],
        ),
        template(
            "string_concat_in_loop",
            "String concatenation in loop",
            "An addition into a value inside a counting loop",
            "for ($type$ $index$ = 0; $index$ < $limit$; $index$++) { $target$ += $value$; }",
            None,
            vec![
                variable("type", None),
                variable("index", None),
                variable("limit", None),
                variable("target", None),
                variable("value", None),
            ],
        ),
        template(
            "system_out_println",
            "System.out.println",
            "A call of println on the standard output",
            "System.out.println($argument$)",
            Some("log.info($argument$)"),
            vec![variable("argument", None)],
        ),
        template(
            "empty_catch",
            "Empty catch blocks",
            "A try whose catch does nothing",
            "try { $body$; } catch ($type$ $exception$) { }",
            None,
            vec![
                variable("body", Some((0, None))),
                variable("type", None),
                variable("exception", None),
            ],
        ),
        template(
            "field_of_type",
            "Field of type",
            "A field whose type the type filter names",
            "$type$ $field$;",
            None,
            vec![variable("type", None), variable("field", None)],
        ),
        template(
            "annotated_methods",
            "Annotated methods",
            "A method with an annotation on it",
            "@$annotation$ $type$ $method$($parameters$) { $body$; }",
            None,
            vec![
                variable("annotation", None),
                variable("type", None),
                variable("method", None),
                variable("parameters", Some((0, None))),
                variable("body", Some((0, None))),
            ],
        ),
        template(
            "new_instance",
            "New instance of class",
            "A constructor call",
            "new $type$($arguments$)",
            None,
            vec![
                variable("type", None),
                variable("arguments", Some((0, None))),
            ],
        ),
        template(
            "instanceof",
            "Instanceof",
            "A check of the type of a value",
            "$value$ instanceof $type$",
            None,
            vec![variable("value", None), variable("type", None)],
        ),
        template(
            "for_each",
            "For each loops",
            "A loop over a collection with a type",
            "for ($type$ $variable$ : $collection$) { $body$; }",
            None,
            vec![
                variable("type", None),
                variable("variable", None),
                variable("collection", None),
                variable("body", Some((1, None))),
            ],
        ),
        template(
            "static_fields",
            "Static fields",
            "A static field",
            "static $type$ $field$;",
            None,
            vec![variable("type", None), variable("field", None)],
        ),
        template(
            "returning_null",
            "Methods returning null",
            "A method whose body only returns null",
            "$type$ $method$($parameters$) { return null; }",
            None,
            vec![
                variable("type", None),
                variable("method", None),
                variable("parameters", Some((0, None))),
            ],
        ),
        template(
            "classes_implementing",
            "Classes implementing interface",
            "A class with an empty body that implements an interface",
            "class $class$ implements $interface$ { }",
            None,
            vec![variable("class", None), variable("interface", None)],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use lang_api::{StructuralQuery, StructuralVariable};

    use super::{ready_templates, replace, simple_type_name, variables};

    fn query(pattern: &str) -> StructuralQuery {
        StructuralQuery {
            pattern: pattern.to_owned(),
            ..StructuralQuery::default()
        }
    }

    fn with(pattern: &str, filters: Vec<StructuralVariable>) -> StructuralQuery {
        StructuralQuery {
            pattern: pattern.to_owned(),
            variables: filters,
            ..StructuralQuery::default()
        }
    }

    fn text_filter(name: &str, text: &str) -> StructuralVariable {
        StructuralVariable {
            text: Some(text.to_owned()),
            ..StructuralVariable::named(name)
        }
    }

    fn count_filter(name: &str, min: usize, max: Option<usize>) -> StructuralVariable {
        StructuralVariable {
            count: Some((min, max)),
            ..StructuralVariable::named(name)
        }
    }

    fn find(query: &StructuralQuery, text: &str) -> Vec<String> {
        super::find(query, text)
            .expect("the template parses")
            .into_iter()
            .map(|found| found.text)
            .collect()
    }

    #[test]
    fn a_repeated_variable_matches_the_same_text_twice() {
        let text = "class A {\n  boolean f(int x, int y) {\n    return x == x;\n  }\n}\n";
        assert_eq!(find(&query("$a$ == $a$"), text), ["x == x"]);
        let other = "class A {\n  boolean f(int x, int y) {\n    return x == y;\n  }\n}\n";
        assert!(find(&query("$a$ == $a$"), other).is_empty());
    }

    #[test]
    fn a_call_is_matched_by_its_whole_shape() {
        let text = "class A {\n  void f() {\n    System.out.println(one);\n    System.out.println(two);\n    System.err.println(one);\n  }\n}\n";
        assert_eq!(
            find(&query("System.out.println($a$)"), text),
            ["System.out.println(one)", "System.out.println(two)"]
        );
    }

    #[test]
    fn comments_and_spaces_do_not_count() {
        let text = "class A {\n  void f() {\n    call( /* it */ first );\n  }\n}\n";
        assert_eq!(find(&query("call($a$)"), text), ["call( /* it */ first )"]);
    }

    #[test]
    fn the_count_of_a_variable_covers_a_sequence() {
        let text =
            "class A {\n  void f() {\n    call();\n    call(one);\n    call(one, two);\n  }\n}\n";
        let any = with("call($args$)", vec![count_filter("args", 0, None)]);
        assert_eq!(find(&any, text), ["call()", "call(one)", "call(one, two)"]);
        let one = query("call($args$)");
        assert_eq!(find(&one, text), ["call(one)"]);
        let two = with("call($args$)", vec![count_filter("args", 2, Some(2))]);
        assert_eq!(find(&two, text), ["call(one, two)"]);
    }

    #[test]
    fn the_count_of_statements_covers_a_block() {
        let text = "class A {\n  void f(boolean b) {\n    if (b) {\n    }\n    if (b) {\n      one();\n    }\n    if (b) {\n      one();\n      two();\n    }\n  }\n}\n";
        let any = with("if ($c$) { $body$; }", vec![count_filter("body", 0, None)]);
        assert_eq!(find(&any, text).len(), 3);
        let one = query("if ($c$) { $body$; }");
        assert_eq!(find(&one, text).len(), 1);
    }

    #[test]
    fn the_order_of_modifiers_does_not_matter() {
        let text = "class A {\n  public static final int value = 1;\n  private int other;\n}\n";
        let pattern = query("static public $type$ $name$;");
        assert_eq!(find(&pattern, text).len(), 1);
        let absent = query("volatile $type$ $name$;");
        assert!(find(&absent, text).is_empty());
    }

    #[test]
    fn a_text_filter_narrows_and_can_be_inverted() {
        let text = "class A {\n  void f() {\n    log(owner);\n    log(pet);\n  }\n}\n";
        let owner = with("log($value$)", vec![text_filter("value", "^owner$")]);
        assert_eq!(find(&owner, text), ["log(owner)"]);
        let not_owner = with("log($value$)", vec![text_filter("value", "!^owner$")]);
        assert_eq!(find(&not_owner, text), ["log(pet)"]);
    }

    #[test]
    fn a_type_filter_reads_the_declaration_next_to_the_place() {
        let text = "class A {\n  void f() {\n    OwnerRepository owners = null;\n    log(owners);\n    String name = null;\n    log(name);\n  }\n}\n";
        let wanted = StructuralVariable {
            type_pattern: Some("^Owner".to_owned()),
            ..StructuralVariable::named("value")
        };
        let found = super::find(&with("log($value$)", vec![wanted]), text).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "log(owners)");
        assert!(found[0].notes.is_empty());
    }

    #[test]
    fn a_type_without_a_declaration_passes_with_a_note() {
        let text = "class A {\n  void f() {\n    log(other());\n  }\n}\n";
        let filter = StructuralVariable {
            type_pattern: Some("Owner".to_owned()),
            ..StructuralVariable::named("value")
        };
        let found = super::find(&with("log($value$)", vec![filter]), text).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].notes, ["type not checked: $value$"]);
    }

    #[test]
    fn a_replacement_keeps_the_indentation_and_the_arguments() {
        let text = "class A {\n    void f() {\n        System.out.println(one);\n    }\n}\n";
        let mut query = query("System.out.println($a$)");
        query.replacement = Some("log.info($a$);\nlog.debug($a$);".to_owned());
        let edits = replace(&query, text).unwrap();
        assert_eq!(edits.len(), 1);
        let edited = format!(
            "{}{}{}",
            &text[..edits[0].start],
            edits[0].replacement,
            &text[edits[0].end..]
        );
        assert!(
            edited.contains("        log.info(one);\n        log.debug(one);"),
            "{edited}"
        );

        let mut args = with("call($args$)", vec![count_filter("args", 0, None)]);
        args.replacement = Some("other($args$)".to_owned());
        let edits = replace(&args, "class A { void f() { call(one, two); } }").unwrap();
        assert_eq!(edits[0].replacement, "other(one, two)");
    }

    #[test]
    fn a_variable_of_the_replacement_must_be_in_the_template() {
        let mut query = query("log($value$)");
        query.replacement = Some("warn($other$)".to_owned());
        assert!(replace(&query, "class A { void f() { log(x); } }").is_err());
    }

    #[test]
    fn the_variables_come_in_the_order_of_the_template() {
        assert_eq!(
            variables("for ($type$ $variable$ : $collection$) { $body$; }").unwrap(),
            ["type", "variable", "collection", "body"]
        );
    }

    #[test]
    fn every_ready_template_parses_and_has_its_sample() {
        let templates = ready_templates();
        assert!(templates.len() >= 15, "{}", templates.len());
        let samples = [
            (
                "method_calls",
                "class A { void f() { one.two(); } }",
                "class A { void f() { one(); } }",
            ),
            (
                "if_statements",
                "class A { void f(boolean b) { if (b) { one(); } } }",
                "class A { void f() { } }",
            ),
            (
                "try_without_finally",
                "class A { void f() { try { one(); } catch (Exception e) { two(); } } }",
                "class A { void f() { try { one(); } catch (Exception e) { two(); } finally { three(); } } }",
            ),
            (
                "equals_with_null",
                "class A { boolean f(String s) { return s.equals(null); } }",
                "class A { boolean f(String s) { return s.equals(o); } }",
            ),
            (
                "string_concat_in_loop",
                "class A { void f() { for (int i = 0; i < 10; i++) { text += i; } } }",
                "class A { void f() { for (int i = 0; i < 10; i++) { text = i; } } }",
            ),
            (
                "system_out_println",
                "class A { void f() { System.out.println(x); } }",
                "class A { void f() { System.out.print(x); } }",
            ),
            (
                "empty_catch",
                "class A { void f() { try { one(); } catch (Exception e) { } } }",
                "class A { void f() { try { one(); } catch (Exception e) { two(); } } }",
            ),
            (
                "field_of_type",
                "class A { int value; }",
                "class A { void value() { } }",
            ),
            (
                "annotated_methods",
                "class A { @Override public void f() { one(); } }",
                "class A { public void f() { one(); } }",
            ),
            (
                "new_instance",
                "class A { Object f() { return new Thing(); } }",
                "class A { Object f() { return thing; } }",
            ),
            (
                "instanceof",
                "class A { boolean f(Object o) { return o instanceof String; } }",
                "class A { boolean f(Object o) { return o == null; } }",
            ),
            (
                "for_each",
                "class A { void f() { for (String s : list) { one(); } } }",
                "class A { void f() { for (int i = 0; i < 2; i++) { one(); } } }",
            ),
            (
                "static_fields",
                "class A { static int value; }",
                "class A { int value; }",
            ),
            (
                "returning_null",
                "class A { Object f() { return null; } }",
                "class A { Object f() { return new Object(); } }",
            ),
            (
                "classes_implementing",
                "class A implements B { }",
                "class A { }",
            ),
        ];
        assert_eq!(
            samples.len(),
            templates.len(),
            "a sample for every template"
        );
        for template in &templates {
            let (_, matching, other) = samples
                .iter()
                .find(|(id, _, _)| *id == template.id)
                .unwrap_or_else(|| panic!("no sample for {}", template.id));
            let query = StructuralQuery {
                pattern: template.pattern.clone(),
                replacement: template.replacement.clone(),
                variables: template.variables.clone(),
            };
            assert!(
                !super::find(&query, matching).unwrap().is_empty(),
                "{} finds its sample",
                template.id
            );
            assert!(
                super::find(&query, other).unwrap().is_empty(),
                "{} does not match the other sample",
                template.id
            );
        }
    }

    #[test]
    fn an_annotation_filter_narrows_annotated_methods() {
        let text = "class A {\n  @GetMapping public void one() { }\n  @PostMapping public void two() { }\n}\n";
        let mut template = ready_templates()
            .into_iter()
            .find(|template| template.id == "annotated_methods")
            .unwrap();
        if let Some(annotation) = template
            .variables
            .iter_mut()
            .find(|variable| variable.name == "annotation")
        {
            annotation.text = Some("GetMapping".to_owned());
        }
        let query = StructuralQuery {
            pattern: template.pattern,
            replacement: None,
            variables: template.variables,
        };
        let found = super::find(&query, text).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].text.contains("@GetMapping"));
    }

    #[test]
    fn the_simple_name_of_a_type_drops_its_package_and_arguments() {
        assert_eq!(simple_type_name("java.util.List<Owner>"), "List");
        assert_eq!(simple_type_name("Owner[]"), "Owner");
    }
}

#[cfg(test)]
mod corpus_tests {
    use std::path::{Path, PathBuf};

    use lang_api::StructuralQuery;

    use super::ready_templates;

    fn java_files(root: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                java_files(&path, files);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "java")
            {
                files.push(path);
            }
        }
    }

    fn count(query: &StructuralQuery, files: &[PathBuf]) -> usize {
        files
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .map(|text| super::find(query, &text).unwrap().len())
            .sum()
    }

    /// The numbers are for DS-REPORT: the corpus is read-only and this test runs once.
    #[test]
    #[ignore = "reads the petclinic project of IDE_CORPUS; run with --ignored"]
    fn petclinic_annotated_methods_and_owner_repository_fields() {
        let corpus = std::env::var("IDE_CORPUS").expect("IDE_CORPUS is set");
        let root = Path::new(&corpus).join("src").join("petclinic");
        let mut files = Vec::new();
        java_files(&root, &mut files);
        assert!(!files.is_empty(), "petclinic is in the corpus");

        let mut annotated = ready_templates()
            .into_iter()
            .find(|template| template.id == "annotated_methods")
            .unwrap();
        if let Some(annotation) = annotated
            .variables
            .iter_mut()
            .find(|variable| variable.name == "annotation")
        {
            annotation.text = Some("GetMapping".to_owned());
        }
        let query = StructuralQuery {
            pattern: annotated.pattern,
            replacement: None,
            variables: annotated.variables,
        };
        let get_mappings = count(&query, &files);

        let mut fields = ready_templates()
            .into_iter()
            .find(|template| template.id == "field_of_type")
            .unwrap();
        if let Some(kind) = fields
            .variables
            .iter_mut()
            .find(|variable| variable.name == "type")
        {
            kind.type_pattern = Some("^OwnerRepository$".to_owned());
        }
        let mut filters = fields.variables;
        for variable in &mut filters {
            variable.count = None;
        }
        let query = StructuralQuery {
            pattern: fields.pattern,
            replacement: None,
            variables: filters,
        };
        let owner_repository_fields = count(&query, &files);

        println!("petclinic: {get_mappings} annotated methods with @GetMapping");
        println!("petclinic: {owner_repository_fields} fields of type OwnerRepository");
        assert!(get_mappings > 0);
        assert!(owner_repository_fields > 0);
    }
}
