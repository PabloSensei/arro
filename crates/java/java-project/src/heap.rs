//! Heap dumps: the streaming reader of the format the runtime writes, the graph of objects
//! it holds and the numbers the window shows. The file is read in one pass from a buffered
//! stream, and the objects are kept in arrays: a dump of several gigabytes holds millions
//! of them.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use base::program::executable;
use base::{Cancel, Result};
use lang_api::{
    HeapClass, HeapField, HeapObject, HeapProgress, HeapSnapshot, HeapString, HeapValue,
};
use proc_sup::ProcessSpec;
use project_model::ProjectModel;

use crate::profile::source_of;
use crate::{BuildEnv, Jdk};

/// The largest record of a file the reader accepts. A segment of a runtime is far below
/// it; a longer one means the file is broken or not a heap dump at all.
const MAX_RECORD: u64 = 512 * 1024 * 1024;
/// The bytes a string shows before the reader stops copying its characters.
const MAX_TEXT: usize = 1 << 20;
/// The elements of an array the window shows.
const SHOWN_ELEMENTS: usize = 16;
/// How often the progress of a long reading is reported.
const PROGRESS_STEP: u64 = 64 * 1024 * 1024;
/// How often a long loop looks at the cancellation flag.
const CANCEL_STEP: u64 = 4096;
/// The bytes of the header of an instance without its fields, as the dump sizes it.
const ARRAY_HEADER: u64 = 16;
/// The mark of a node without a dominator or a parent.
const NONE: u32 = u32::MAX;

const TAG_STRING: u8 = 0x01;
const TAG_LOAD_CLASS: u8 = 0x02;
const TAG_HEAP_DUMP: u8 = 0x0C;
const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;

const ROOT_UNKNOWN: u8 = 0xFF;
const ROOT_JNI_GLOBAL: u8 = 0x01;
const ROOT_JNI_LOCAL: u8 = 0x02;
const ROOT_JAVA_FRAME: u8 = 0x03;
const ROOT_NATIVE_STACK: u8 = 0x04;
const ROOT_STICKY_CLASS: u8 = 0x05;
const ROOT_THREAD_BLOCK: u8 = 0x06;
const ROOT_MONITOR_USED: u8 = 0x07;
const ROOT_THREAD_OBJECT: u8 = 0x08;
const ROOT_INTERNED_STRING: u8 = 0x89;
const ROOT_FINALIZING: u8 = 0x8A;
const ROOT_DEBUGGER: u8 = 0x8B;
const ROOT_REFERENCE_CLEANUP: u8 = 0x8C;
const ROOT_VM_INTERNAL: u8 = 0x8D;
const ROOT_JNI_MONITOR: u8 = 0x8E;
const ROOT_UNREACHABLE: u8 = 0x90;
const CLASS_DUMP: u8 = 0x20;
const INSTANCE_DUMP: u8 = 0x21;
const OBJ_ARRAY_DUMP: u8 = 0x22;
const PRIM_ARRAY_DUMP: u8 = 0x23;
const HEAP_DUMP_INFO: u8 = 0xFE;

/// Writes the heap of the running program `pid` into `file` with the `jcmd` of `jdk`.
/// Blocks, so it belongs on a background thread.
pub fn capture(
    env: &BuildEnv,
    jdk: &Jdk,
    pid: u32,
    file: &Path,
    model: &ProjectModel,
    cancel: &Cancel,
) -> Result<()> {
    let jcmd = jdk.home.join("bin").join(executable("jcmd"));
    if !jcmd.is_file() {
        return Err(format!(
            "the JDK of the program has no jcmd ({}), so the heap cannot be dumped",
            jcmd.display()
        )
        .into());
    }
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let spec = env
        .apply(ProcessSpec::new(&jcmd), jdk)
        .cwd(model.root.clone())
        .args([
            pid.to_string(),
            "GC.heap_dump".to_owned(),
            file.display().to_string(),
        ]);
    let captured = proc_sup::capture(&spec, cancel).map_err(|error| error.to_string())?;
    if !captured.status.success() {
        let reason = match captured.stderr.trim().is_empty() {
            true => captured.stdout,
            false => captured.stderr,
        };
        return Err(format!("jcmd did not write the heap dump: {}", reason.trim()).into());
    }
    if !file.is_file() {
        return Err(format!("jcmd answered but wrote no heap dump at {}", file.display()).into());
    }
    Ok(())
}

/// Reads a heap dump of `path` into the model of the window. The source of a class of the
/// project comes from `model`.
pub fn read(
    path: &Path,
    model: &ProjectModel,
    cancel: &Cancel,
    progress: &mut dyn FnMut(HeapProgress),
) -> Result<HeapSnapshot> {
    let file = File::open(path)
        .map_err(|error| format!("cannot open the heap dump {}: {error}", path.display()))?;
    let total = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let mut input = Input::new(BufReader::with_capacity(1 << 20, file));
    let mut dump = Dump::new();
    dump.read(&mut input, cancel, progress, total)
        .map_err(|error| format!("the heap dump is cut or broken: {error}"))?;
    dump.finish(path, model, cancel, progress, total)
        .map_err(|error| format!("the heap dump is cut or broken: {error}").into())
}

/// Reads the `jcmd` line of a running program into a file with the JDK of the project.
pub fn capture_of(
    env: &BuildEnv,
    pid: u32,
    file: &Path,
    model: &ProjectModel,
    cancel: &Cancel,
) -> Result<()> {
    let jdk = env.project_jdk(&model.root)?;
    capture(env, &jdk, pid, file, model, cancel)
}

/// The number of bytes one element of a primitive array takes, `None` for an object.
fn element_size(kind: u8) -> Option<usize> {
    match kind {
        4 | 8 => Some(1),
        5 | 9 => Some(2),
        6 | 10 => Some(4),
        7 | 11 => Some(8),
        _ => None,
    }
}

/// The descriptor of a primitive array with elements of `kind`.
fn primitive_name(kind: u8) -> String {
    match kind {
        4 => "boolean[]".to_owned(),
        5 => "char[]".to_owned(),
        6 => "float[]".to_owned(),
        7 => "double[]".to_owned(),
        8 => "byte[]".to_owned(),
        9 => "short[]".to_owned(),
        10 => "int[]".to_owned(),
        11 => "long[]".to_owned(),
        other => format!("array of {other}"),
    }
}

/// The width of a value of `kind` in the file, `None` for an object reference.
fn value_size(kind: u8, id_size: u32) -> Option<usize> {
    match kind {
        2 => Some(id_size as usize),
        4 | 8 => Some(1),
        5 | 9 => Some(2),
        6 | 10 => Some(4),
        7 | 11 => Some(8),
        _ => None,
    }
}

/// The name the window shows for a class name of the dump: slashes become dots.
fn display_name(raw: &str) -> String {
    if let Some(component) = raw
        .strip_prefix("[L")
        .and_then(|name| name.strip_suffix(';'))
    {
        return format!("{}[]", display_name(component));
    }
    if raw.len() == 2 && raw.starts_with('[') {
        return primitive_display(raw.as_bytes()[1]).to_owned();
    }
    raw.replace('/', ".")
}

fn primitive_display(kind: u8) -> &'static str {
    match kind {
        b'Z' => "boolean[]",
        b'C' => "char[]",
        b'F' => "float[]",
        b'D' => "double[]",
        b'B' => "byte[]",
        b'S' => "short[]",
        b'I' => "int[]",
        b'J' => "long[]",
        _ => "array",
    }
}

/// The package of a class name, empty for an array and the default package.
fn package_of(name: &str) -> String {
    if name.starts_with('[') {
        return String::new();
    }
    name.rsplit_once('.')
        .map_or_else(String::new, |(package, _)| package.to_owned())
}

/// One field of a class as the dump describes it.
#[derive(Clone, Copy, Debug)]
struct FieldDef {
    name: u64,
    kind: u8,
}

/// A class of the dump: the fields of its instances and its superclass.
#[derive(Clone, Debug, Default)]
struct ClassDef {
    name_id: Option<u64>,
    super_id: u64,
    instance_size: u64,
    fields: Vec<FieldDef>,
}

/// A value read from the file before the addresses are resolved to objects.
#[derive(Clone, Debug)]
enum RawValue {
    Null,
    Object(u64),
    Bool(bool),
    Char(char),
    Float(f32),
    Double(f64),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
}

/// What one object record describes.
#[derive(Clone, Debug)]
enum RawKind {
    Instance {
        class_id: u64,
        fields: Vec<(u64, RawValue)>,
    },
    /// An instance whose class dump comes later in the file.
    Pending {
        class_id: u64,
        bytes: Vec<u8>,
    },
    ObjectArray {
        class_id: u64,
        elements: Vec<u64>,
    },
    PrimitiveArray {
        element: u8,
        count: u64,
        /// The offset of the elements in the file, read again for the characters of a
        /// string without keeping the array itself.
        offset: Option<u64>,
        preview: String,
    },
}

#[derive(Clone, Debug)]
struct RawObject {
    address: u64,
    shallow: u64,
    kind: RawKind,
}

/// The state of one reading of a file.
struct Dump {
    id_size: u32,
    strings: HashMap<u64, Arc<str>>,
    class_names: HashMap<u64, u64>,
    classes: HashMap<u64, ClassDef>,
    static_roots: HashSet<u64>,
    objects: Vec<RawObject>,
    roots: HashSet<u64>,
    objects_read: u64,
}

impl Dump {
    fn new() -> Self {
        Self {
            id_size: 0,
            strings: HashMap::new(),
            class_names: HashMap::new(),
            classes: HashMap::new(),
            static_roots: HashSet::new(),
            objects: Vec::new(),
            roots: HashSet::new(),
            objects_read: 0,
        }
    }

    fn read(
        &mut self,
        input: &mut Input<BufReader<File>>,
        cancel: &Cancel,
        progress: &mut dyn FnMut(HeapProgress),
        total: u64,
    ) -> Result<()> {
        let magic = input.until_nul(64)?;
        let magic = String::from_utf8_lossy(&magic).into_owned();
        if !magic.starts_with("JAVA PROFILE 1.0.") {
            return Err(format!(
                "this is not a heap dump: the file starts with {magic:?}, not with a profile header"
            )
            .into());
        }
        if !matches!(magic.as_str(), "JAVA PROFILE 1.0.1" | "JAVA PROFILE 1.0.2") {
            return Err(format!("the heap dump has an unsupported version: {magic}").into());
        }
        self.id_size = input.u32()?;
        if self.id_size != 4 && self.id_size != 8 {
            return Err(format!(
                "the heap dump uses {}-byte object identifiers, only 4 and 8 are known",
                self.id_size
            )
            .into());
        }
        let _timestamp = input.u64()?;
        let mut reported = 0u64;
        loop {
            cancel.check()?;
            let tag = match input.u8() {
                Ok(tag) => tag,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(truncated(input.bytes_read, error)),
            };
            if tag == TAG_HEAP_DUMP {
                // The old record carries no length: the sub-records follow to the end.
                let mut body = StreamBody { input };
                self.heap(&mut body, cancel)?;
                break;
            }
            let _time = input.u32()?;
            let length = u64::from(input.u32()?);
            if length > MAX_RECORD {
                return Err(format!(
                    "the heap dump has a record of {length} bytes, which is too long to read"
                )
                .into());
            }
            match tag {
                TAG_STRING => {
                    if length < u64::from(self.id_size) {
                        return Err("a string record is shorter than its identifier".into());
                    }
                    let id = input.id(self.id_size)?;
                    let text = input.text((length - u64::from(self.id_size)) as usize)?;
                    self.strings.insert(id, Arc::from(text));
                }
                TAG_LOAD_CLASS => {
                    let mut body = LimitedBody {
                        input,
                        left: length,
                    };
                    let _serial = body.u32()?;
                    let class_id = body.id(self.id_size)?;
                    let _stack = body.u32()?;
                    let name_id = body.id(self.id_size)?;
                    self.class_names.insert(class_id, name_id);
                    let rest = body.remaining();
                    body.skip(rest)?;
                }
                TAG_HEAP_DUMP_SEGMENT => {
                    let mut body = LimitedBody {
                        input,
                        left: length,
                    };
                    self.heap(&mut body, cancel)?;
                    let rest = body.remaining();
                    body.skip(rest)?;
                }
                _ => input.skip(length)?,
            }
            if input.bytes_read.saturating_sub(reported) >= PROGRESS_STEP {
                reported = input.bytes_read;
                progress(HeapProgress {
                    bytes: input.bytes_read,
                    total,
                    objects: self.objects_read,
                });
            }
        }
        progress(HeapProgress {
            bytes: input.bytes_read,
            total,
            objects: self.objects_read,
        });
        Ok(())
    }

    fn heap(&mut self, body: &mut dyn Body, cancel: &Cancel) -> Result<()> {
        loop {
            body.check_cancelled(cancel)?;
            let tag = match body.u8() {
                Ok(tag) => tag,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let id_size = self.id_size;
            match tag {
                ROOT_UNKNOWN
                | ROOT_STICKY_CLASS
                | ROOT_MONITOR_USED
                | ROOT_INTERNED_STRING
                | ROOT_FINALIZING
                | ROOT_DEBUGGER
                | ROOT_REFERENCE_CLEANUP
                | ROOT_VM_INTERNAL
                | ROOT_UNREACHABLE => {
                    let id = body.id(id_size)?;
                    self.roots.insert(id);
                }
                ROOT_JNI_GLOBAL => {
                    let id = body.id(id_size)?;
                    let _global = body.id(id_size)?;
                    self.roots.insert(id);
                }
                ROOT_JNI_LOCAL | ROOT_JAVA_FRAME => {
                    let id = body.id(id_size)?;
                    let _thread = body.u32()?;
                    let _frame = body.u32()?;
                    self.roots.insert(id);
                }
                ROOT_NATIVE_STACK => {
                    let id = body.id(id_size)?;
                    let _thread = body.u32()?;
                    self.roots.insert(id);
                }
                ROOT_THREAD_BLOCK => {
                    let id = body.id(id_size)?;
                    let _thread = body.u32()?;
                    self.roots.insert(id);
                }
                ROOT_THREAD_OBJECT => {
                    let id = body.id(id_size)?;
                    let _thread = body.u32()?;
                    let _stack = body.u32()?;
                    self.roots.insert(id);
                }
                ROOT_JNI_MONITOR => {
                    let id = body.id(id_size)?;
                    let _thread = body.u32()?;
                    let _frame = body.u32()?;
                    self.roots.insert(id);
                }
                CLASS_DUMP => self.class_dump(body)?,
                INSTANCE_DUMP => self.instance(body)?,
                OBJ_ARRAY_DUMP => self.object_array(body)?,
                PRIM_ARRAY_DUMP => self.primitive_array(body)?,
                HEAP_DUMP_INFO => {
                    let _heap = body.u32()?;
                    let _name = body.id(id_size)?;
                }
                other => {
                    return Err(format!(
                        "the heap dump has an unknown record 0x{other:02X}, so the rest of it cannot be read"
                    )
                    .into());
                }
            }
        }
    }

    fn class_dump(&mut self, body: &mut dyn Body) -> Result<()> {
        let id = body.id(self.id_size)?;
        let _stack = body.u32()?;
        let super_id = body.id(self.id_size)?;
        for _ in 0..5 {
            let _ = body.id(self.id_size)?;
        }
        let instance_size = u64::from(body.u32()?);
        let mut class = ClassDef {
            name_id: self.class_names.get(&id).copied(),
            super_id,
            instance_size,
            fields: Vec::new(),
        };
        let constants = body.u16()?;
        for _ in 0..constants {
            let _index = body.u16()?;
            let kind = body.u8()?;
            let _ = read_value(body, kind, self.id_size)?;
        }
        let statics = body.u16()?;
        for _ in 0..statics {
            let _name = body.id(self.id_size)?;
            let kind = body.u8()?;
            if let RawValue::Object(address) = read_value(body, kind, self.id_size)?
                && address != 0
            {
                self.static_roots.insert(address);
            }
        }
        let fields = body.u16()?;
        class.fields.reserve(fields as usize);
        for _ in 0..fields {
            let name = body.id(self.id_size)?;
            let kind = body.u8()?;
            class.fields.push(FieldDef { name, kind });
        }
        self.classes.entry(id).or_insert(class);
        Ok(())
    }

    fn instance(&mut self, body: &mut dyn Body) -> Result<()> {
        let address = body.id(self.id_size)?;
        let _stack = body.u32()?;
        let class_id = body.id(self.id_size)?;
        let length = body.u32()? as usize;
        // The bytes of an instance hold the fields of its class and then of every
        // superclass. The class dumps of the superclasses may still be ahead in the file,
        // so the bytes wait until every class of the chain is known.
        let complete = self
            .classes
            .get(&class_id)
            .is_some_and(|class| chain_complete(&self.classes, class));
        if !complete {
            let bytes = body.bytes(length as u64)?;
            self.push(RawObject {
                address,
                shallow: 0,
                kind: RawKind::Pending { class_id, bytes },
            });
            return Ok(());
        }
        let class = self
            .classes
            .get(&class_id)
            .expect("the chain of a known class starts at its class");
        let fields = decode_fields(
            &self.classes,
            class,
            &body.bytes(length as u64)?,
            self.id_size,
        )?;
        let shallow = class.instance_size;
        self.push(RawObject {
            address,
            shallow,
            kind: RawKind::Instance { class_id, fields },
        });
        Ok(())
    }

    fn object_array(&mut self, body: &mut dyn Body) -> Result<()> {
        let address = body.id(self.id_size)?;
        let _stack = body.u32()?;
        let count = body.u32()? as usize;
        let class_id = body.id(self.id_size)?;
        let mut elements = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            elements.push(body.id(self.id_size)?);
        }
        let shallow = (count as u64 + 1) * u64::from(self.id_size) + ARRAY_HEADER;
        self.push(RawObject {
            address,
            shallow,
            kind: RawKind::ObjectArray { class_id, elements },
        });
        Ok(())
    }

    fn primitive_array(&mut self, body: &mut dyn Body) -> Result<()> {
        let address = body.id(self.id_size)?;
        let _stack = body.u32()?;
        let count = u64::from(body.u32()?);
        let element = body.u8()?;
        let Some(size) = element_size(element) else {
            return Err(format!("a primitive array has an unknown element type {element}").into());
        };
        let offset = body.offset();
        let mut preview = String::new();
        let shown = count.min(SHOWN_ELEMENTS as u64);
        for index in 0..shown {
            if index > 0 {
                preview.push_str(", ");
            }
            match element {
                4 => preview.push_str(match body.u8()? {
                    0 => "false",
                    _ => "true",
                }),
                5 => {
                    let value = body.u16()?;
                    preview.push_str(
                        &char::from_u32(u32::from(value))
                            .unwrap_or('\u{FFFD}')
                            .escape_default()
                            .to_string(),
                    );
                }
                6 => preview.push_str(&f32::from_bits(body.u32()?).to_string()),
                7 => preview.push_str(&f64::from_bits(body.u64()?).to_string()),
                8 => preview.push_str(&(body.u8()? as i8).to_string()),
                9 => preview.push_str(&(body.u16()? as i16).to_string()),
                10 => preview.push_str(&(body.u32()? as i32).to_string()),
                11 => preview.push_str(&(body.u64()? as i64).to_string()),
                _ => unreachable!("the element size of every other kind is unknown"),
            }
        }
        if count > shown {
            preview.push_str("...");
        }
        let left = (count - shown) * size as u64;
        body.skip(left)?;
        let shallow = count * size as u64 + ARRAY_HEADER;
        self.push(RawObject {
            address,
            shallow,
            kind: RawKind::PrimitiveArray {
                element,
                count,
                offset,
                preview,
            },
        });
        Ok(())
    }

    fn push(&mut self, object: RawObject) {
        self.objects_read += 1;
        self.objects.push(object);
    }

    fn finish(
        self,
        path: &Path,
        model: &ProjectModel,
        cancel: &Cancel,
        progress: &mut dyn FnMut(HeapProgress),
        total: u64,
    ) -> Result<HeapSnapshot> {
        let Dump {
            id_size,
            strings,
            classes,
            static_roots,
            class_names,
            mut objects,
            roots,
            objects_read,
        } = self;
        resolve_pending(&mut objects, &classes, id_size)?;
        objects.sort_by_key(|object| object.address);
        if objects
            .windows(2)
            .any(|pair| pair[0].address == pair[1].address)
        {
            return Err("the heap dump describes one object twice".into());
        }
        let addresses = AddressBook::new(&objects);

        // The names of the classes, with their superclasses, decide which field is a
        // weak reference and which class a row shows.
        let mut class_by_id: HashMap<u64, usize> = HashMap::new();
        let mut defs: Vec<(u64, &ClassDef)> = classes.iter().map(|(id, def)| (*id, def)).collect();
        defs.sort_by_key(|(id, _)| *id);
        let mut names: Vec<String> = Vec::with_capacity(defs.len());
        for (index, (id, def)) in defs.iter().enumerate() {
            class_by_id.insert(*id, index);
            // A class dump usually comes after its load class record, but the name is
            // known even when the records of one class arrive in the other order.
            let name_id = def.name_id.or_else(|| class_names.get(id).copied());
            let name = name_id
                .and_then(|name| strings.get(&name))
                .map_or_else(|| format!("class@{id}"), |name| display_name(name));
            names.push(name);
        }
        let super_names: Vec<Option<String>> = defs
            .iter()
            .map(|(_, def)| {
                class_by_id
                    .get(&def.super_id)
                    .and_then(|index| names.get(*index))
                    .cloned()
            })
            .collect();
        let mut weak: HashSet<usize> = names
            .iter()
            .position(|name| name == "java.lang.ref.Reference")
            .into_iter()
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for (index, super_name) in super_names.iter().enumerate() {
                if weak.contains(&index) {
                    continue;
                }
                if let Some(super_name) = super_name
                    && names
                        .iter()
                        .position(|name| name == super_name)
                        .is_some_and(|super_index| weak.contains(&super_index))
                {
                    weak.insert(index);
                    changed = true;
                }
            }
        }

        // A string object keeps its characters outside itself: the reader takes the array
        // again from the file, so a dump of many strings does not sit twice in memory.
        let string_texts = read_strings(
            path,
            &objects,
            &addresses,
            &classes,
            &class_by_id,
            &names,
            &strings,
        )?;

        let mut class_map: HashMap<String, u32> = HashMap::new();
        let mut class_rows: Vec<HeapClass> = Vec::new();
        let mut snapshot_objects: Vec<HeapObject> = Vec::with_capacity(objects.len());
        let mut edges: Vec<(u32, u32)> = Vec::new();
        let mut root_indices: Vec<u32> = Vec::new();
        let mut total_bytes = 0u64;

        for (index, object) in objects.iter().enumerate() {
            if index % CANCEL_STEP as usize == 0 {
                cancel.check()?;
            }
            let (class_name, is_weak) = match &object.kind {
                RawKind::Instance { class_id, .. } => {
                    let found = class_by_id.get(class_id).copied();
                    let name = found
                        .and_then(|class| names.get(class))
                        .cloned()
                        .unwrap_or_else(|| format!("class@{class_id}"));
                    (name, found.is_some_and(|class| weak.contains(&class)))
                }
                RawKind::Pending { class_id, .. } => (format!("class@{class_id}"), false),
                RawKind::ObjectArray { class_id, .. } => {
                    let name = class_names_of(
                        &classes,
                        &class_names,
                        &strings,
                        &class_by_id,
                        &names,
                        *class_id,
                    )
                    .unwrap_or_else(|| "Object[]".to_owned());
                    (name, false)
                }
                RawKind::PrimitiveArray { element, .. } => (primitive_name(*element), false),
            };
            let class = match class_map.get(&class_name) {
                Some(index) => *index,
                None => {
                    let is_array = class_name.starts_with('[');
                    class_rows.push(HeapClass {
                        name: class_name.clone(),
                        package: package_of(&class_name),
                        count: 0,
                        shallow: 0,
                        retained: 0,
                        source: if is_array {
                            None
                        } else {
                            source_of(model, &class_name)
                        },
                    });
                    let index = (class_rows.len() - 1) as u32;
                    class_map.insert(class_name.clone(), index);
                    index
                }
            };
            let mut fields = Vec::new();
            match &object.kind {
                RawKind::Instance { fields: raw, .. } => {
                    for (name, value) in raw {
                        let name = strings.get(name).map_or_else(|| Arc::from("?"), Arc::clone);
                        fields.push((name, heap_value(value, &addresses)));
                    }
                }
                RawKind::Pending { .. } => {}
                RawKind::ObjectArray { elements, .. } => {
                    for (index, address) in elements.iter().take(SHOWN_ELEMENTS).enumerate() {
                        let value = match address {
                            0 => HeapValue::Null,
                            address => match addresses.get(*address) {
                                Some(target) => HeapValue::Object(target),
                                None => HeapValue::Address(*address),
                            },
                        };
                        fields.push((Arc::from(format!("[{index}]")), value));
                    }
                    if elements.len() > SHOWN_ELEMENTS {
                        fields.push((
                            Arc::from("..."),
                            HeapValue::Array(format!(
                                "{} more elements",
                                elements.len() - SHOWN_ELEMENTS
                            )),
                        ));
                    }
                }
                RawKind::PrimitiveArray { preview, .. } => {
                    if !preview.is_empty() {
                        fields.push((Arc::from("elements"), HeapValue::Array(preview.clone())));
                    }
                }
            }
            let is_root = roots.contains(&object.address) || static_roots.contains(&object.address);
            if is_root {
                root_indices.push(index as u32);
            }
            total_bytes += object.shallow;
            let class_row = &mut class_rows[class as usize];
            class_row.count += 1;
            class_row.shallow += object.shallow;
            let converted: Vec<HeapField> = fields
                .into_iter()
                .map(|(name, value)| HeapField { name, value })
                .collect();
            // The graph follows every reference of the object, not only the fields and
            // elements the window shows, so a path and a retained size cover all of them.
            match &object.kind {
                RawKind::Instance { fields: raw, .. } => {
                    for (name, value) in raw {
                        if is_weak && strings.get(name).is_some_and(|name| &**name == "referent") {
                            continue;
                        }
                        if let RawValue::Object(address) = value
                            && let Some(target) = addresses.get(*address)
                        {
                            edges.push((index as u32, target));
                        }
                    }
                }
                RawKind::ObjectArray { elements, .. } => {
                    for address in elements {
                        if let Some(target) = addresses.get(*address) {
                            edges.push((index as u32, target));
                        }
                    }
                }
                RawKind::Pending { .. } | RawKind::PrimitiveArray { .. } => {}
            }
            snapshot_objects.push(HeapObject {
                address: object.address,
                class,
                shallow: object.shallow,
                retained: 0,
                dominator: None,
                parent: None,
                root: is_root,
                text: string_texts.get(index).cloned().flatten(),
                fields: converted,
            });
        }

        let shallow: Vec<u64> = snapshot_objects
            .iter()
            .map(|object| object.shallow)
            .collect();
        let analysis = analyse(&shallow, &root_indices, &edges, cancel)?;
        let count = snapshot_objects.len() as u32;
        for (index, object) in snapshot_objects.iter_mut().enumerate() {
            object.retained = analysis.retained[index];
            object.dominator = (analysis.dominator[index] != NONE
                && analysis.dominator[index] != count
                && analysis.dominator[index] != index as u32)
                .then_some(analysis.dominator[index]);
            object.parent = (analysis.parent[index] != NONE).then_some(analysis.parent[index]);
            if let Some(class) = class_rows.get_mut(object.class as usize) {
                class.retained = class.retained.saturating_add(object.retained);
            }
        }
        let mut strings_rows: HashMap<Arc<str>, (usize, u64, u64)> = HashMap::new();
        for object in &snapshot_objects {
            let Some(text) = &object.text else {
                continue;
            };
            let entry = strings_rows
                .entry(text.clone())
                .or_insert((0, 0, object.shallow));
            entry.0 += 1;
            entry.1 += object.shallow;
        }
        let mut strings: Vec<HeapString> = strings_rows
            .into_iter()
            .filter(|(_, (count, _, _))| *count > 1)
            .map(|(text, (count, shallow, one))| HeapString {
                text,
                count,
                shallow: one,
                wasted: shallow.saturating_sub(one),
            })
            .collect();
        strings.sort_by(|left, right| {
            right
                .wasted
                .cmp(&left.wasted)
                .then_with(|| left.text.cmp(&right.text))
        });

        progress(HeapProgress {
            bytes: total,
            total,
            objects: objects_read,
        });
        Ok(HeapSnapshot {
            classes: class_rows,
            objects: snapshot_objects,
            strings,
            total_bytes,
            roots: root_indices.len(),
            truncated: false,
        })
    }
}

/// A value of an instance field with its object references resolved.
fn heap_value(value: &RawValue, addresses: &AddressBook) -> HeapValue {
    match value {
        RawValue::Null => HeapValue::Null,
        RawValue::Object(address) => match addresses.get(*address) {
            Some(target) => HeapValue::Object(target),
            None => HeapValue::Address(*address),
        },
        RawValue::Bool(value) => HeapValue::Bool(*value),
        RawValue::Char(value) => HeapValue::Char(*value),
        RawValue::Float(value) => HeapValue::Real(f64::from(*value)),
        RawValue::Double(value) => HeapValue::Real(*value),
        RawValue::Byte(value) => HeapValue::Number(i64::from(*value)),
        RawValue::Short(value) => HeapValue::Number(i64::from(*value)),
        RawValue::Int(value) => HeapValue::Number(i64::from(*value)),
        RawValue::Long(value) => HeapValue::Number(*value),
    }
}

fn class_names_of(
    classes: &HashMap<u64, ClassDef>,
    class_names: &HashMap<u64, u64>,
    strings: &HashMap<u64, Arc<str>>,
    class_by_id: &HashMap<u64, usize>,
    names: &[String],
    class_id: u64,
) -> Option<String> {
    class_by_id
        .get(&class_id)
        .and_then(|index| names.get(*index))
        .cloned()
        .or_else(|| {
            class_names
                .get(&class_id)
                .and_then(|name| strings.get(name))
                .or_else(|| {
                    classes
                        .get(&class_id)
                        .and_then(|def| def.name_id)
                        .and_then(|name| strings.get(&name))
                })
                .map(|name| display_name(name))
        })
}

/// Whether every superclass of `class` has its class dump in `classes`.
fn chain_complete(classes: &HashMap<u64, ClassDef>, class: &ClassDef) -> bool {
    let mut current = Some(class);
    for _ in 0..1024 {
        let Some(def) = current else {
            return false;
        };
        if def.super_id == 0 {
            return true;
        }
        current = classes.get(&def.super_id);
    }
    false
}

/// Decodes the bytes of one instance with the fields of its class and its superclasses,
/// in the order the dump writes them: the class first, then every superclass.
fn decode_fields(
    classes: &HashMap<u64, ClassDef>,
    class: &ClassDef,
    bytes: &[u8],
    id_size: u32,
) -> Result<Vec<(u64, RawValue)>> {
    let mut at = 0usize;
    let mut fields = Vec::with_capacity(class.fields.len());
    let mut current = Some(class);
    for _ in 0..1024 {
        let Some(def) = current else {
            break;
        };
        for field in &def.fields {
            let Some(size) = value_size(field.kind, id_size) else {
                return Err(format!("a field has an unknown type {}", field.kind).into());
            };
            if at + size > bytes.len() {
                return Err("an instance is shorter than the fields of its class".into());
            }
            let value = value_at(field.kind, &bytes[at..at + size], id_size);
            at += size;
            fields.push((field.name, value));
        }
        if def.super_id == 0 {
            break;
        }
        current = classes.get(&def.super_id);
    }
    Ok(fields)
}

/// One value from the bytes of an instance.
fn value_at(kind: u8, bytes: &[u8], id_size: u32) -> RawValue {
    match kind {
        2 => {
            let address = if id_size == 4 {
                u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            } else {
                u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])
            };
            match address {
                0 => RawValue::Null,
                _ => RawValue::Object(address),
            }
        }
        4 => RawValue::Bool(bytes[0] != 0),
        5 => RawValue::Char(
            char::from_u32(u32::from(u16::from_be_bytes([bytes[0], bytes[1]])))
                .unwrap_or('\u{FFFD}'),
        ),
        6 => RawValue::Float(f32::from_bits(u32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        7 => RawValue::Double(f64::from_bits(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))),
        8 => RawValue::Byte(bytes[0] as i8),
        9 => RawValue::Short(i16::from_be_bytes([bytes[0], bytes[1]])),
        10 => RawValue::Int(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        11 => RawValue::Long(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => RawValue::Null,
    }
}

/// Reads a value from a class dump: the constant pool and the static fields share the
/// encoding of the fields of an instance.
fn read_value(body: &mut dyn Body, kind: u8, id_size: u32) -> Result<RawValue> {
    Ok(match kind {
        2 => match body.id(id_size)? {
            0 => RawValue::Null,
            address => RawValue::Object(address),
        },
        4 => RawValue::Bool(body.u8()? != 0),
        5 => RawValue::Char(char::from_u32(u32::from(body.u16()?)).unwrap_or('\u{FFFD}')),
        6 => RawValue::Float(f32::from_bits(body.u32()?)),
        7 => RawValue::Double(f64::from_bits(body.u64()?)),
        8 => RawValue::Byte(body.u8()? as i8),
        9 => RawValue::Short(body.u16()? as i16),
        10 => RawValue::Int(body.u32()? as i32),
        11 => RawValue::Long(body.u64()? as i64),
        other => return Err(format!("a value has an unknown type {other}").into()),
    })
}

/// Decodes the instances whose class dump followed them in the file.
fn resolve_pending(
    objects: &mut [RawObject],
    classes: &HashMap<u64, ClassDef>,
    id_size: u32,
) -> Result<()> {
    for object in objects {
        let RawKind::Pending { class_id, bytes } = &object.kind else {
            continue;
        };
        let Some(class) = classes.get(class_id) else {
            return Err(format!(
                "an instance names a class the dump does not describe: {}",
                class_id
            )
            .into());
        };
        if !chain_complete(classes, class) {
            return Err(format!(
                "an instance names a superclass the dump does not describe: {}",
                class.super_id
            )
            .into());
        }
        let fields = decode_fields(classes, class, bytes, id_size)?;
        let shallow = class.instance_size;
        object.kind = RawKind::Instance {
            class_id: *class_id,
            fields,
        };
        object.shallow = shallow;
    }
    Ok(())
}

/// The characters of every string object: the array of a string is read again from the
/// file, because the dump keeps it as a separate object.
#[allow(clippy::too_many_arguments)]
fn read_strings(
    path: &Path,
    objects: &[RawObject],
    addresses: &AddressBook,
    classes: &HashMap<u64, ClassDef>,
    class_by_id: &HashMap<u64, usize>,
    names: &[String],
    strings: &HashMap<u64, Arc<str>>,
) -> Result<Vec<Option<Arc<str>>>> {
    let mut texts: Vec<Option<Arc<str>>> = vec![None; objects.len()];
    let mut file: Option<File> = None;
    for (index, object) in objects.iter().enumerate() {
        let RawKind::Instance { class_id, fields } = &object.kind else {
            continue;
        };
        let Some(class) = class_by_id.get(class_id) else {
            continue;
        };
        if names
            .get(*class)
            .is_none_or(|name| name != "java.lang.String")
        {
            continue;
        }
        let Some(class_def) = classes.get(class_id) else {
            continue;
        };
        let field_name = |wanted: &str| -> Option<u64> {
            class_def
                .fields
                .iter()
                .map(|field| field.name)
                .find(|id| strings.get(id).is_some_and(|name| &**name == wanted))
        };
        let Some(value_name) = field_name("value") else {
            continue;
        };
        let Some((_, RawValue::Object(value_address))) =
            fields.iter().find(|(name, _)| *name == value_name)
        else {
            continue;
        };
        let Some(value_index) = addresses.get(*value_address) else {
            continue;
        };
        let value_object = &objects[value_index as usize];
        let RawKind::PrimitiveArray {
            element,
            count,
            offset: Some(offset),
            ..
        } = &value_object.kind
        else {
            continue;
        };
        if *element != 8 && *element != 5 {
            continue;
        }
        let utf16 = field_name("coder").is_some_and(|name| {
            fields
                .iter()
                .find(|(field, _)| *field == name)
                .is_some_and(|(_, value)| matches!(value, RawValue::Byte(1)))
        });
        let width = element_size(*element).unwrap_or(1);
        let wanted = (*count as usize).saturating_mul(width);
        let bytes_to_read = wanted.min(MAX_TEXT * 2);
        if bytes_to_read == 0 {
            texts[index] = Some(Arc::from(""));
            continue;
        }
        if file.is_none() {
            file = Some(File::open(path).map_err(|error| {
                format!("cannot read the heap dump again for its strings: {error}")
            })?);
        }
        let file = file.as_mut().expect("the file was just opened");
        file.seek(SeekFrom::Start(*offset))?;
        let mut bytes = vec![0u8; bytes_to_read];
        file.read_exact(&mut bytes)?;
        let text = match (*element, utf16) {
            (8, false) => bytes.iter().map(|byte| *byte as char).collect::<String>(),
            (8, true) | (5, _) => {
                let (pairs, _) = bytes.as_chunks::<2>();
                let units: Vec<u16> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();
                String::from_utf16_lossy(&units)
            }
            _ => continue,
        };
        texts[index] = Some(Arc::from(trim_text(text, wanted > bytes_to_read)));
    }
    Ok(texts)
}

fn trim_text(text: String, cut: bool) -> String {
    let mut text: String = text.chars().take(MAX_TEXT).collect();
    if cut {
        text.push('…');
    }
    text
}

/// The addresses of the objects in the order of the snapshot, for the lookups of a
/// reference.
struct AddressBook {
    addresses: Vec<u64>,
}

impl AddressBook {
    fn new(objects: &[RawObject]) -> Self {
        Self {
            addresses: objects.iter().map(|object| object.address).collect(),
        }
    }

    fn get(&self, address: u64) -> Option<u32> {
        self.addresses
            .binary_search(&address)
            .ok()
            .map(|index| index as u32)
    }
}

/// What the analysis of a graph gives: the dominator of each object and the bytes it
/// retains, and the previous object of a shortest path from a root.
pub struct Analysis {
    pub dominator: Vec<u32>,
    pub retained: Vec<u64>,
    pub parent: Vec<u32>,
}

/// The dominators and the retained sizes of the graph, with a synthetic node in front of
/// every root. The iterative algorithm of Cooper, Harvey and Kennedy: the nodes in reverse
/// postorder, the intersection walking the current dominator tree.
pub fn analyse(
    shallow: &[u64],
    roots: &[u32],
    edges: &[(u32, u32)],
    cancel: &Cancel,
) -> Result<Analysis> {
    let count = shallow.len();
    let root = count as u32;
    let nodes = count + 1;

    let mut out_degree = vec![0u32; nodes];
    for (from, _) in edges {
        out_degree[*from as usize] += 1;
    }
    out_degree[root as usize] = roots.len() as u32;
    let mut offsets = vec![0usize; nodes + 1];
    for node in 0..nodes {
        offsets[node + 1] = offsets[node] + out_degree[node] as usize;
    }
    let mut targets = vec![0u32; offsets[nodes]];
    let mut fill = offsets.clone();
    for (from, to) in edges {
        targets[fill[*from as usize]] = *to;
        fill[*from as usize] += 1;
    }
    for root_index in roots {
        targets[fill[root as usize]] = *root_index;
        fill[root as usize] += 1;
    }

    // A depth first walk gives the reverse postorder the algorithm visits the nodes in.
    let mut seen = vec![false; nodes];
    let mut postorder = Vec::with_capacity(nodes);
    let mut stack: Vec<(u32, usize)> = Vec::with_capacity(1024);
    seen[root as usize] = true;
    stack.push((root, offsets[root as usize]));
    let mut steps = 0u64;
    while let Some((node, at)) = stack.last_mut() {
        steps += 1;
        if steps.is_multiple_of(CANCEL_STEP) {
            cancel.check()?;
        }
        let end = offsets[*node as usize + 1];
        if *at < end {
            let target = targets[*at];
            *at += 1;
            if !seen[target as usize] {
                seen[target as usize] = true;
                stack.push((target, offsets[target as usize]));
            }
        } else {
            postorder.push(*node);
            stack.pop();
        }
    }
    let mut order: Vec<u32> = postorder.into_iter().rev().collect();
    let mut position = vec![NONE; nodes];
    for (index, node) in order.iter().enumerate() {
        position[*node as usize] = index as u32;
    }

    // The predecessors of every node, for the intersection below.
    let mut in_degree = vec![0u32; nodes];
    for (_, to) in edges {
        in_degree[*to as usize] += 1;
    }
    for root_index in roots {
        in_degree[*root_index as usize] += 1;
    }
    let mut pred_offsets = vec![0usize; nodes + 1];
    for node in 0..nodes {
        pred_offsets[node + 1] = pred_offsets[node] + in_degree[node] as usize;
    }
    let mut predecessors = vec![0u32; pred_offsets[nodes]];
    let mut pred_fill = pred_offsets.clone();
    for (from, to) in edges {
        predecessors[pred_fill[*to as usize]] = *from;
        pred_fill[*to as usize] += 1;
    }
    for root_index in roots {
        predecessors[pred_fill[*root_index as usize]] = root;
        pred_fill[*root_index as usize] += 1;
    }

    let mut dominator = vec![NONE; nodes];
    dominator[root as usize] = root;
    let mut changed = true;
    let mut rounds = 0u64;
    while changed {
        changed = false;
        rounds += 1;
        if rounds.is_multiple_of(64) {
            cancel.check()?;
        }
        for node in order.iter().skip(1) {
            let mut new = NONE;
            let from = pred_offsets[*node as usize];
            let to = pred_offsets[*node as usize + 1];
            for &predecessor in &predecessors[from..to] {
                if dominator[predecessor as usize] == NONE {
                    continue;
                }
                new = match new {
                    NONE => predecessor,
                    current => intersect(predecessor, current, &dominator, &position),
                };
            }
            if new != NONE && dominator[*node as usize] != new {
                dominator[*node as usize] = new;
                changed = true;
            }
        }
    }
    for (node, value) in dominator.iter_mut().enumerate().take(count) {
        if *value == NONE {
            *value = node as u32;
        }
    }

    let mut retained = shallow.to_vec();
    for node in order.iter().skip(1).rev() {
        let parent = dominator[*node as usize];
        if parent != NONE && parent != root {
            retained[parent as usize] += retained[*node as usize];
        }
    }

    // The shortest reference path from any root, which a path to the root follows back.
    let mut parent = vec![NONE; count];
    let mut queued = vec![false; count];
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    for root_index in roots {
        if !queued[*root_index as usize] {
            queued[*root_index as usize] = true;
            queue.push_back(*root_index);
        }
    }
    while let Some(node) = queue.pop_front() {
        steps += 1;
        if steps.is_multiple_of(CANCEL_STEP) {
            cancel.check()?;
        }
        let from = offsets[node as usize];
        let to = offsets[node as usize + 1];
        for &target in &targets[from..to] {
            if !queued[target as usize] {
                queued[target as usize] = true;
                parent[target as usize] = node;
                queue.push_back(target);
            }
        }
    }

    order.clear();
    Ok(Analysis {
        dominator,
        retained,
        parent,
    })
}

fn intersect(mut left: u32, mut right: u32, dominator: &[u32], position: &[u32]) -> u32 {
    while left != right {
        while position[left as usize] > position[right as usize] {
            left = dominator[left as usize];
        }
        while position[right as usize] > position[left as usize] {
            right = dominator[right as usize];
        }
    }
    left
}

fn truncated(offset: u64, error: io::Error) -> base::Error {
    format!("the heap dump ends unexpectedly after {offset} bytes: {error}").into()
}

/// A reader of the file that counts the bytes it passed.
struct Input<R> {
    inner: R,
    bytes_read: u64,
}

impl<R: Read> Input<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buffer)?;
        self.bytes_read += buffer.len() as u64;
        Ok(())
    }

    fn u8(&mut self) -> io::Result<u8> {
        let mut buffer = [0u8; 1];
        self.read(&mut buffer)?;
        Ok(buffer[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        let mut buffer = [0u8; 2];
        self.read(&mut buffer)?;
        Ok(u16::from_be_bytes(buffer))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let mut buffer = [0u8; 4];
        self.read(&mut buffer)?;
        Ok(u32::from_be_bytes(buffer))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let mut buffer = [0u8; 8];
        self.read(&mut buffer)?;
        Ok(u64::from_be_bytes(buffer))
    }

    fn id(&mut self, size: u32) -> io::Result<u64> {
        match size {
            4 => self.u32().map(u64::from),
            _ => self.u64(),
        }
    }

    /// The bytes up to a zero byte, the first line of the file.
    fn until_nul(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() < limit {
            let byte = match self.u8() {
                Ok(byte) => byte,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            };
            if byte == 0 {
                break;
            }
            out.push(byte);
        }
        Ok(out)
    }

    /// A record body of `length` bytes, as the records of a fixed size have it.
    fn body(&mut self, length: u64) -> io::Result<Vec<u8>> {
        let length = usize::try_from(length)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the record is too long"))?;
        let mut body = vec![0u8; length];
        self.read(&mut body)?;
        Ok(body)
    }

    fn text(&mut self, length: usize) -> io::Result<String> {
        let mut bytes = vec![0u8; length];
        self.read(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn skip(&mut self, length: u64) -> io::Result<()> {
        if length <= 8192 {
            let mut buffer = [0u8; 8192];
            self.read(&mut buffer[..length as usize])
        } else {
            let mut buffer = vec![0u8; 64 * 1024];
            let mut left = length;
            while left > 0 {
                let take = left.min(buffer.len() as u64) as usize;
                self.read(&mut buffer[..take])?;
                left -= take as u64;
            }
            Ok(())
        }
    }
}

/// The body of a heap record: a segment in memory, or the rest of the file.
trait Body: Read {
    fn u8(&mut self) -> io::Result<u8>;
    fn u16(&mut self) -> io::Result<u16>;
    fn u32(&mut self) -> io::Result<u32>;
    fn u64(&mut self) -> io::Result<u64>;
    fn id(&mut self, size: u32) -> io::Result<u64>;
    fn bytes(&mut self, length: u64) -> io::Result<Vec<u8>>;
    /// Skips the rest of a record whose first bytes were read.
    fn skip(&mut self, length: u64) -> io::Result<()> {
        let mut left = length;
        let mut buffer = [0u8; 8192];
        while left > 0 {
            let take = left.min(buffer.len() as u64) as usize;
            self.read_exact(&mut buffer[..take])?;
            left -= take as u64;
        }
        Ok(())
    }
    /// Where the next byte is in the file, when the body knows it.
    fn offset(&self) -> Option<u64>;
    fn check_cancelled(&self, cancel: &Cancel) -> Result<()> {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        Ok(())
    }
}

fn read_u8<R: Read>(reader: &mut R) -> io::Result<u8> {
    let mut buffer = [0u8; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

fn read_u16<R: Read>(reader: &mut R) -> io::Result<u16> {
    let mut buffer = [0u8; 2];
    reader.read_exact(&mut buffer)?;
    Ok(u16::from_be_bytes(buffer))
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buffer = [0u8; 4];
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_be_bytes(buffer))
}

fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut buffer = [0u8; 8];
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_be_bytes(buffer))
}

fn read_id<R: Read>(reader: &mut R, size: u32) -> io::Result<u64> {
    match size {
        4 => read_u32(reader).map(u64::from),
        _ => read_u64(reader),
    }
}

fn read_bytes<R: Read>(reader: &mut R, length: u64) -> io::Result<Vec<u8>> {
    let length = usize::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the record is too long"))?;
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// The body of a record of an announced length, read straight from the file: a segment of
/// several megabytes never sits in memory as a whole, and the arrays inside it are skipped
/// without a copy.
struct LimitedBody<'a, R> {
    input: &'a mut Input<R>,
    left: u64,
}

impl<R: Read> LimitedBody<'_, R> {
    fn remaining(&self) -> u64 {
        self.left
    }
}

impl<R: Read> Read for LimitedBody<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let take = (buffer.len() as u64).min(self.left) as usize;
        self.input.read(&mut buffer[..take])?;
        self.left -= take as u64;
        Ok(take)
    }
}

impl<R: Read> Body for LimitedBody<'_, R> {
    fn u8(&mut self) -> io::Result<u8> {
        read_u8(self)
    }

    fn u16(&mut self) -> io::Result<u16> {
        read_u16(self)
    }

    fn u32(&mut self) -> io::Result<u32> {
        read_u32(self)
    }

    fn u64(&mut self) -> io::Result<u64> {
        read_u64(self)
    }

    fn id(&mut self, size: u32) -> io::Result<u64> {
        read_id(self, size)
    }

    fn bytes(&mut self, length: u64) -> io::Result<Vec<u8>> {
        if length > self.left {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a record is shorter than one of its values",
            ));
        }
        read_bytes(self, length)
    }

    fn offset(&self) -> Option<u64> {
        Some(self.input.bytes_read)
    }
}

/// The old heap record, which runs to the end of the file.
struct StreamBody<'a, R> {
    input: &'a mut Input<R>,
}

impl<R: Read> Read for StreamBody<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.input.inner.read(buffer)
    }
}

impl<R: Read> Body for StreamBody<'_, R> {
    fn u8(&mut self) -> io::Result<u8> {
        self.input.u8()
    }

    fn u16(&mut self) -> io::Result<u16> {
        self.input.u16()
    }

    fn u32(&mut self) -> io::Result<u32> {
        self.input.u32()
    }

    fn u64(&mut self) -> io::Result<u64> {
        self.input.u64()
    }

    fn id(&mut self, size: u32) -> io::Result<u64> {
        self.input.id(size)
    }

    fn bytes(&mut self, length: u64) -> io::Result<Vec<u8>> {
        let bytes = self.input.body(length)?;
        Ok(bytes)
    }

    fn skip(&mut self, length: u64) -> io::Result<()> {
        self.input.skip(length)
    }

    fn offset(&self) -> Option<u64> {
        Some(self.input.bytes_read)
    }
}
