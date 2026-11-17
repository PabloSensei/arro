//! The HTTP handlers a project exposes, as a language reads them from its sources. The
//! core knows nothing about the annotations of a framework: it shows what a language
//! reports and turns it into a request.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// The method of an HTTP handler. `Any` is a handler that answers every method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
    Any,
}

impl HttpMethod {
    /// The methods a filter offers, the one that matches everything first.
    pub const ALL: [Self; 8] = [
        Self::Any,
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Delete,
        Self::Patch,
        Self::Head,
        Self::Options,
    ];

    /// The word of a request line. A handler without a method answers a `GET`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Any => "ANY",
        }
    }

    /// The method a generated request asks with: `ANY` asks with `GET`.
    pub const fn request_method(self) -> Self {
        match self {
            Self::Any => Self::Get,
            other => other,
        }
    }

    /// The method of a word of a source or of a request line.
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str().eq_ignore_ascii_case(word))
    }
}

/// What a handler expects in its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointParameterKind {
    /// A part of the path between braces, `{ownerId}`.
    Path,
    /// A name and a value after the question mark.
    Query,
    /// The text of the request.
    Body,
}

/// One parameter of a handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointParameter {
    pub name: String,
    pub kind: EndpointParameterKind,
    /// The declared type of a body, when the language names it.
    pub type_name: Option<String>,
}

/// One HTTP handler of a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub method: HttpMethod,
    /// The path as the code spells it, `/owners/{ownerId}`.
    pub path: String,
    /// The type that declares the handler, `demo.OwnerController`.
    pub class: String,
    /// The source the handler is in.
    pub file: PathBuf,
    /// The line of the handler name in the source, from zero.
    pub line: u32,
    /// The byte offsets of the handler name in the source.
    pub name_range: Range<usize>,
    pub parameters: Vec<EndpointParameter>,
}

impl Endpoint {
    /// The name of the type without its package, `OwnerController`.
    pub fn class_name(&self) -> &str {
        self.class.rsplit('.').next().unwrap_or(&self.class)
    }

    /// The path with every `{name}` written as the place of a request file, `{{name}}`.
    pub fn request_path(&self) -> String {
        self.path.replace('{', "{{").replace('}', "}}")
    }

    /// The parameters of a kind, in the order the handler declares them.
    pub fn params(&self, kind: EndpointParameterKind) -> impl Iterator<Item = &EndpointParameter> {
        self.parameters
            .iter()
            .filter(move |parameter| parameter.kind == kind)
    }
}

/// What a language found in one file: its handlers and the port their server listens on,
/// when the project config names one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndpointScan {
    pub port: Option<u16>,
    pub endpoints: Vec<Endpoint>,
}

/// The handlers of one source file, in the order they appear. Blocks, so callers run it
/// off the UI thread.
pub trait EndpointProvider: Send + Sync {
    fn file_endpoints(&self, path: &Path, text: &str) -> EndpointScan;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(method: HttpMethod, path: &str, parameters: Vec<EndpointParameter>) -> Endpoint {
        Endpoint {
            method,
            path: path.to_owned(),
            class: "demo.OwnerController".to_owned(),
            file: PathBuf::from("C:/p/OwnerController.txt"),
            line: 4,
            name_range: 0..4,
            parameters,
        }
    }

    #[test]
    fn a_method_has_its_word_and_any_asks_with_get() {
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Any.as_str(), "ANY");
        assert_eq!(HttpMethod::Any.request_method(), HttpMethod::Get);
        assert_eq!(HttpMethod::parse("delete"), Some(HttpMethod::Delete));
        assert_eq!(HttpMethod::parse("TRACE"), None);
    }

    #[test]
    fn a_path_turns_its_parts_into_request_variables() {
        let found = endpoint(HttpMethod::Get, "/owners/{ownerId}", Vec::new());
        assert_eq!(found.request_path(), "/owners/{{ownerId}}");
        assert_eq!(found.class_name(), "OwnerController");
    }

    #[test]
    fn parameters_of_a_kind_keep_their_order() {
        let found = endpoint(
            HttpMethod::Post,
            "/owners",
            vec![
                EndpointParameter {
                    name: "name".to_owned(),
                    kind: EndpointParameterKind::Query,
                    type_name: None,
                },
                EndpointParameter {
                    name: "body".to_owned(),
                    kind: EndpointParameterKind::Body,
                    type_name: Some("Owner".to_owned()),
                },
                EndpointParameter {
                    name: "city".to_owned(),
                    kind: EndpointParameterKind::Query,
                    type_name: None,
                },
            ],
        );
        let names: Vec<&str> = found
            .params(EndpointParameterKind::Query)
            .map(|parameter| parameter.name.as_str())
            .collect();
        assert_eq!(names, ["name", "city"]);
        assert_eq!(
            found
                .params(EndpointParameterKind::Body)
                .next()
                .and_then(|parameter| parameter.type_name.as_deref()),
            Some("Owner")
        );
    }
}
