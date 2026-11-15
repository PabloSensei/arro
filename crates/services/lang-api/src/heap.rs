//! A snapshot of a heap: the classes of the objects, the bytes each object holds and the
//! way its references lead to the roots of the runtime. The language reads the file and
//! counts the numbers; the window sorts them and draws the rows.

use std::path::PathBuf;
use std::sync::Arc;

/// How much of a heap dump a reader has passed. A dump of several gigabytes has no length
/// its writer announces, so `total` is the size of the file when the system knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapProgress {
    pub bytes: u64,
    pub total: u64,
    pub objects: u64,
}

/// One class of a heap with the bytes of its instances.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeapClass {
    /// The name as the dump spells it, `demo.Main` or `[B`.
    pub name: String,
    /// The package part, `demo`, empty for an array or the default package.
    pub package: String,
    pub count: usize,
    /// The bytes of the instances themselves, without what they reference.
    pub shallow: u64,
    /// The bytes of the objects this class dominates, the instances included.
    pub retained: u64,
    /// The source of a class of the project, absent for the runtime and libraries.
    pub source: Option<PathBuf>,
}

impl HeapClass {
    /// The class of a package the window marks as the project's own.
    pub fn is_own(&self) -> bool {
        self.source.is_some()
    }
}

/// A value of a field or of an element of an array.
#[derive(Clone, Debug, PartialEq)]
pub enum HeapValue {
    /// A field that holds no reference.
    Null,
    /// A reference to an object of the same snapshot.
    Object(u32),
    /// An address the dump mentions but does not describe, such as a class object.
    Address(u64),
    /// The characters of a string.
    Text(Arc<str>),
    Bool(bool),
    Char(char),
    /// An integer of every width, a byte, a short, an int or a long.
    Number(i64),
    /// A float or a double.
    Real(f64),
    /// The first elements of a primitive array, separated by commas.
    Array(String),
}

/// One field of an instance or one element of an array.
#[derive(Clone, Debug, PartialEq)]
pub struct HeapField {
    pub name: Arc<str>,
    pub value: HeapValue,
}

/// One object of the heap with the bytes it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeapObject {
    /// The identifier the dump gives the object.
    pub address: u64,
    /// The index of its [`HeapClass`].
    pub class: u32,
    pub shallow: u64,
    /// The bytes of the objects this one dominates, itself included.
    pub retained: u64,
    /// The object that dominates this one, `None` for a root and for an object no root
    /// reaches.
    pub dominator: Option<u32>,
    /// The previous object of a shortest reference path from a root, `None` for a root
    /// and for an object no root reaches.
    pub parent: Option<u32>,
    /// A garbage collector root itself, such as a thread object or a static field.
    pub root: bool,
    /// The characters of a string object.
    pub text: Option<Arc<str>>,
    pub fields: Vec<HeapField>,
}

/// Strings of the heap with the same characters, for the hint about the memory they take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeapString {
    pub text: Arc<str>,
    pub count: usize,
    /// What one copy of the string object holds, the characters included.
    pub shallow: u64,
    /// The bytes every copy after the first one takes, as the hint counts them.
    pub wasted: u64,
}

/// Everything one reading of a heap dump holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeapSnapshot {
    pub classes: Vec<HeapClass>,
    pub objects: Vec<HeapObject>,
    /// Strings with equal characters, the most memory first.
    pub strings: Vec<HeapString>,
    /// The sum of the shallow bytes of every object.
    pub total_bytes: u64,
    /// How many objects the runtime named as garbage collector roots.
    pub roots: usize,
    /// Only a part of the file was read, so the window shows a part of the heap.
    pub truncated: bool,
}

impl HeapSnapshot {
    pub fn class_of(&self, object: u32) -> Option<&HeapClass> {
        let object = self.objects.get(object as usize)?;
        self.classes.get(object.class as usize)
    }

    /// The references from the nearest root to `object`, the root first. Empty for a root
    /// or for an object no root reaches.
    pub fn path(&self, object: u32) -> Vec<u32> {
        let mut path = Vec::new();
        let mut current = Some(object);
        let mut left = self.objects.len();
        while let Some(index) = current {
            path.push(index);
            current = self.objects.get(index as usize).and_then(|it| it.parent);
            left = left.saturating_sub(1);
            if left == 0 {
                break;
            }
        }
        path.reverse();
        path
    }

    /// The text of a value, as a row of the window shows it.
    pub fn value_text(&self, value: &HeapValue) -> String {
        match value {
            HeapValue::Null => "null".to_owned(),
            HeapValue::Object(index) => match self.objects.get(*index as usize) {
                Some(object) if object.text.is_some() => {
                    format!("\"{}\"", object.text.as_deref().unwrap_or_default())
                }
                Some(object) => format!(
                    "{} @{}",
                    self.class_of(*index)
                        .map_or("?", |class| class.name.as_str()),
                    object.address
                ),
                None => "?".to_owned(),
            },
            HeapValue::Address(address) => format!("@{address}"),
            HeapValue::Text(text) => format!("\"{text}\""),
            HeapValue::Bool(value) => value.to_string(),
            HeapValue::Char(value) => value.escape_default().to_string(),
            HeapValue::Number(value) => value.to_string(),
            HeapValue::Real(value) => value.to_string(),
            HeapValue::Array(text) => text.clone(),
        }
    }
}
