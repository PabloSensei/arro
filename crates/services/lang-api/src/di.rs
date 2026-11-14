//! The dependency-injection model of a project: the beans a language finds in its
//! sources, the points the framework fills with them, and the checks of those points.
//! The core knows nothing about the annotations of a framework: it shows the places a
//! language reports and turns a badge click into a navigation.

use std::ops::Range;
use std::path::PathBuf;

use crate::Provider;

/// One place a badge or a check goes to: a declaration or a point of an injection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectionPlace {
    /// The text of the place in the list of a badge, `OwnerController.owners`.
    pub label: String,
    pub file: PathBuf,
    /// The byte offset of the name in the source.
    pub offset: usize,
}

/// A bean the project declares: a component, a factory method or a repository interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bean {
    /// The name the framework knows the bean by.
    pub name: String,
    /// The type without its package, for a list.
    pub type_name: String,
    /// The type with its package.
    pub qualified_type: String,
    /// What marks the declaration as a bean, such as the annotation of a class.
    pub stereotype: String,
    pub file: PathBuf,
    /// The line of the declaration, from zero.
    pub line: u32,
    /// The byte offset of the declaration name in the source.
    pub offset: usize,
    /// The declaration is the primary candidate of its type.
    pub primary: bool,
    /// The places the bean is injected into.
    pub usages: Vec<InjectionPlace>,
}

/// A point that receives a bean: a field, a parameter of a constructor or a parameter
/// of a factory method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectionPoint {
    /// The declared type without its package, for a message.
    pub type_name: String,
    /// The declared type with its package, when the language resolved it.
    pub qualified_type: String,
    pub file: PathBuf,
    /// The line of the point, from zero.
    pub line: u32,
    /// The byte offset of the point name in the source.
    pub offset: usize,
    /// The byte range of the declared type, the place a warning underlines.
    pub range: Range<usize>,
    /// The places of the beans the point receives, after the qualifier and the primary
    /// candidate narrowed them.
    pub candidates: Vec<InjectionPlace>,
}

/// A warning about a point the framework cannot fill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeanWarning {
    pub file: PathBuf,
    /// The byte range of the type to underline.
    pub range: Range<usize>,
    pub message: String,
}

/// Everything the model of a project holds at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BeanModel {
    pub beans: Vec<Bean>,
    pub injections: Vec<InjectionPoint>,
    pub warnings: Vec<BeanWarning>,
}

impl BeanModel {
    /// The beans declared in one file, in name order.
    pub fn beans_of(&self, file: &std::path::Path) -> impl Iterator<Item = &Bean> {
        self.beans.iter().filter(move |bean| bean.file == file)
    }

    /// The points of one file.
    pub fn injections_of(&self, file: &std::path::Path) -> impl Iterator<Item = &InjectionPoint> {
        self.injections
            .iter()
            .filter(move |point| point.file == file)
    }

    /// The warnings of one file.
    pub fn warnings_of(&self, file: &std::path::Path) -> impl Iterator<Item = &BeanWarning> {
        self.warnings
            .iter()
            .filter(move |warning| warning.file == file)
    }
}

/// The question for a whole model: the caller asks once and reads every later update
/// from the same answer stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeanQuery;

/// The model of the beans of a language. The answer stream sends the model again after
/// every change of the sources.
pub trait BeanProvider: Provider<BeanQuery, BeanModel> {
    /// A source of the project changed on disk, so the model is read again.
    fn sources_changed(&self);
}
