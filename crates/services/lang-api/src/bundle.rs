//! The files of a localized set of `.properties` files, as a language sees them.
//!
//! The base file is `<base>.properties`; a locale file is `<base>_<locale>.properties`
//! next to it. A language that knows where the sets of a project are answers with the
//! files of the set a path belongs to; the editor builds the same list from the names in
//! the folder when the language has no answer.

use std::path::PathBuf;

/// One file of a localized set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    pub path: PathBuf,
    /// `None` for the base file, the name of the locale for the others.
    pub locale: Option<String>,
}
