//! The place a declaration of a hover answer is in. The server writes a local variable or a
//! parameter as its declaration, a dash, and the method that declares it, with the package
//! and the class in front of the method name.

use lang_api::HoverPlace;

/// Splits the first line of a hover answer into its declaration and the method it is in,
/// shown as `Class.method()`. A line whose tail is not a method keeps its text.
pub(crate) fn split(line: &str) -> Option<HoverPlace> {
    let (declaration, tail) = line.split_once(" - ")?;
    let declaration = declaration.trim_end();
    if declaration.trim().is_empty() {
        return None;
    }
    let (name, parameters) = tail.trim_end().split_once('(')?;
    let parameters = parameters.strip_suffix(')')?;
    // A dash inside a string of an annotation ends the parameters with a quote.
    if parameters.contains('"') {
        return None;
    }
    let names: Vec<&str> = name.split('.').collect();
    let [.., class, method] = names.as_slice() else {
        return None;
    };
    if !names.iter().all(|name| is_identifier(name)) {
        return None;
    }
    Some(HoverPlace {
        declaration: declaration.to_owned(),
        place: format!("{class}.{method}()"),
    })
}

/// Whether a name is one identifier: a letter, `_` or `$` first, then letters, digits, `_`
/// or `$`.
fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && characters
            .all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_local_variable_keeps_its_declaration_and_names_its_method_short() {
        let place = split(
            "Page<Owner> ownersResults - org.springframework.samples.petclinic.owner.OwnerController.processFindForm(int, Owner, …)",
        )
        .expect("the place of a local variable");
        assert_eq!(place.declaration, "Page<Owner> ownersResults");
        assert_eq!(place.place, "OwnerController.processFindForm()");
    }

    #[test]
    fn generics_arrays_and_varargs_of_the_parameters_do_not_change_the_split() {
        let place = split("String[] names - org.example.Util.join(String[], Object...)")
            .expect("the place of a parameter");
        assert_eq!(place.declaration, "String[] names");
        assert_eq!(place.place, "Util.join()");

        let place = split(
            "Map<String, List<Integer>> index - org.example.Store.lookup(Map<String, List<Integer>>)",
        )
        .expect("the place of a generic parameter");
        assert_eq!(place.declaration, "Map<String, List<Integer>> index");
        assert_eq!(place.place, "Store.lookup()");
    }

    #[test]
    fn a_nested_class_gives_its_own_name_before_the_method() {
        let place = split("int count - org.example.Outer.Inner.bump()").expect("the place");
        assert_eq!(place.place, "Inner.bump()");
    }

    #[test]
    fn a_line_without_a_dash_keeps_its_text() {
        assert_eq!(split("void run()"), None);
    }

    #[test]
    fn a_tail_that_is_not_a_method_keeps_its_text() {
        assert_eq!(split("String name - org.example.Owner"), None);
    }

    #[test]
    fn a_dash_inside_an_annotation_string_is_not_a_place() {
        assert_eq!(split("@Pattern(regexp = \"a - b.c(d)\")"), None);
        assert_eq!(split("@Size(message = \"x - y.z(w)\") String value"), None);
    }

    #[test]
    fn a_line_without_a_declaration_keeps_its_text() {
        assert_eq!(split(" - org.example.Foo.bar()"), None);
    }
}
