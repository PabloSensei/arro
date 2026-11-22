//! The class and the package of a class of a library, read from the address the Java language
//! server gives the class: `jdt://contents/<module>/<package>/<Class>.class?<query>`.

use lang_api::LibraryPlace;

/// The place a class of a library names. Every address with the scheme of the server is one,
/// even when its path has an unknown shape; an address of any other scheme is not.
pub(crate) fn describe(uri: &str) -> Option<LibraryPlace> {
    let rest = uri.strip_prefix("jdt:")?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    // The query names the project and the escaped path of the class, not its place.
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let last = parts.last().copied().unwrap_or("Definition");
    let name = last.strip_suffix(".class").unwrap_or(last).to_owned();
    // `contents/<module>/<package>/<Class>.class`: the package is what stands between the
    // module and the class.
    let container = if parts.len() >= 3 && parts.first() == Some(&"contents") {
        parts[2..parts.len() - 1].join(".")
    } else if parts.len() >= 2 {
        parts[..parts.len() - 1].join(".")
    } else {
        String::new()
    };
    Some(LibraryPlace {
        name,
        container,
        library: true,
    })
}

#[cfg(test)]
mod tests {
    use super::describe;

    #[test]
    fn a_class_of_the_runtime_names_its_class_and_package() {
        let place = describe(
            "jdt://contents/java.base/java.lang/String.class?=demo/%5C%2Fjava.base%3Cjava.lang%28String.class",
        )
        .expect("a library address");
        assert_eq!(place.name, "String");
        assert_eq!(place.container, "java.lang");
        assert!(place.library);
    }

    #[test]
    fn a_raw_slash_in_the_query_does_not_move_the_class() {
        let place = describe(
            "jdt://contents/demo-1.0.jar/com/example/Demo.class?=demo/%5C%2Fdemo-1.0.jar%3Ccom.example(Demo.class",
        )
        .expect("a library address");
        assert_eq!(place.name, "Demo");
        assert_eq!(place.container, "com.example");
    }

    #[test]
    fn an_address_without_contents_takes_the_folders_before_the_class() {
        let place = describe("jdt://Demo.class").expect("a library address");
        assert_eq!(place.name, "Demo");
        assert_eq!(place.container, "");

        let place = describe("jdt://runtime/java/lang/String.class").expect("a library address");
        assert_eq!(place.name, "String");
        assert_eq!(place.container, "runtime.java.lang");
    }

    #[test]
    fn an_address_of_another_scheme_is_not_a_library_address() {
        assert_eq!(describe("file:///c:/work/Main.java"), None);
        assert_eq!(describe("https://example.com/contents/Demo.class"), None);
        assert_eq!(describe("Demo"), None);
    }
}
