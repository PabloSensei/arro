//! The reader of a heap dump on small files written by the tests, and the dominators on
//! the graphs of the textbook examples.

use std::fs;

use base::Cancel;
use java_project::heap::{analyse, read};
use lang_api::{HeapSnapshot, HeapValue};
use project_model::ProjectModel;

const TAG_STRING: u8 = 0x01;
const TAG_LOAD_CLASS: u8 = 0x02;
const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;
const ROOT_THREAD_OBJECT: u8 = 0x08;
const ROOT_JNI_GLOBAL: u8 = 0x01;
const ROOT_JAVA_FRAME: u8 = 0x03;
const ROOT_STICKY_CLASS: u8 = 0x05;
const CLASS_DUMP: u8 = 0x20;
const INSTANCE_DUMP: u8 = 0x21;
const OBJ_ARRAY_DUMP: u8 = 0x22;
const PRIM_ARRAY_DUMP: u8 = 0x23;

/// Writes the records of a small heap dump by hand, the way the format describes them,
/// so a test can name every identifier.
struct Builder {
    ids: u32,
    top: Vec<u8>,
    heap: Vec<u8>,
}

impl Builder {
    fn new(ids: u32) -> Self {
        Self {
            ids,
            top: Vec::new(),
            heap: Vec::new(),
        }
    }

    fn id(&self, value: u64) -> Vec<u8> {
        match self.ids {
            4 => (value as u32).to_be_bytes().to_vec(),
            _ => value.to_be_bytes().to_vec(),
        }
    }

    fn record(&mut self, tag: u8, body: &[u8]) {
        self.top.push(tag);
        self.top.extend_from_slice(&0u32.to_be_bytes());
        self.top
            .extend_from_slice(&(body.len() as u32).to_be_bytes());
        self.top.extend_from_slice(body);
    }

    fn low_record(&mut self, tag: u8, body: &[u8]) {
        self.heap.push(tag);
        self.heap.extend_from_slice(body);
    }

    fn string(&mut self, id: u64, text: &str) {
        let mut body = self.id(id);
        body.extend_from_slice(text.as_bytes());
        self.record(TAG_STRING, &body);
    }

    fn load_class(&mut self, class_id: u64, name_id: u64) {
        let mut body = 0u32.to_be_bytes().to_vec();
        body.extend_from_slice(&self.id(class_id));
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&self.id(name_id));
        self.record(TAG_LOAD_CLASS, &body);
    }

    fn class_dump(
        &mut self,
        class_id: u64,
        super_id: u64,
        instance_size: u32,
        fields: &[(u64, u8)],
    ) {
        let mut body = self.id(class_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&self.id(super_id));
        for _ in 0..5 {
            body.extend_from_slice(&self.id(0));
        }
        body.extend_from_slice(&instance_size.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (name, kind) in fields {
            body.extend_from_slice(&self.id(*name));
            body.push(*kind);
        }
        self.low_record(CLASS_DUMP, &body);
    }

    fn instance(&mut self, address: u64, class_id: u64, bytes: &[u8]) {
        let mut body = self.id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&self.id(class_id));
        body.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        body.extend_from_slice(bytes);
        self.low_record(INSTANCE_DUMP, &body);
    }

    fn object_array(&mut self, address: u64, class_id: u64, elements: &[u64]) {
        let mut body = self.id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(elements.len() as u32).to_be_bytes());
        body.extend_from_slice(&self.id(class_id));
        for element in elements {
            body.extend_from_slice(&self.id(*element));
        }
        self.low_record(OBJ_ARRAY_DUMP, &body);
    }

    fn primitive_array(&mut self, address: u64, element: u8, bytes: &[u8]) {
        let mut body = self.id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        body.push(element);
        body.extend_from_slice(bytes);
        self.low_record(PRIM_ARRAY_DUMP, &body);
    }

    fn root(&mut self, address: u64) {
        let mut body = self.id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        self.low_record(ROOT_THREAD_OBJECT, &body);
    }

    fn root_jni_global(&mut self, address: u64, global: u64) {
        let mut body = self.id(address);
        body.extend_from_slice(&self.id(global));
        self.low_record(ROOT_JNI_GLOBAL, &body);
    }

    fn root_java_frame(&mut self, address: u64) {
        let mut body = self.id(address);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        self.low_record(ROOT_JAVA_FRAME, &body);
    }

    fn root_sticky_class(&mut self, address: u64) {
        let body = self.id(address);
        self.low_record(ROOT_STICKY_CLASS, &body);
    }

    fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"JAVA PROFILE 1.0.2\0");
        out.extend_from_slice(&self.ids.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&self.top);
        out.push(TAG_HEAP_DUMP_SEGMENT);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(self.heap.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.heap);
        out
    }
}

fn object_bytes(value: u64, ids: u32) -> Vec<u8> {
    match ids {
        4 => (value as u32).to_be_bytes().to_vec(),
        _ => value.to_be_bytes().to_vec(),
    }
}

fn snapshot(bytes: &[u8], name: &str) -> HeapSnapshot {
    let path = test_support::scratch(name).join("dump.hprof");
    fs::write(&path, bytes).unwrap();
    read(&path, &ProjectModel::default(), &Cancel::new(), &mut |_| {}).unwrap()
}

fn find(snapshot: &HeapSnapshot, address: u64) -> u32 {
    snapshot
        .objects
        .iter()
        .position(|object| object.address == address)
        .unwrap_or_else(|| panic!("no object at {address:#x}")) as u32
}

#[test]
fn four_byte_identifiers_give_classes_fields_arrays_and_a_path_to_the_root() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.string(2, "value");
    builder.string(3, "[Ldemo/Main;");
    builder.load_class(0x100, 1);
    builder.load_class(0x101, 3);
    builder.class_dump(0x100, 0, 4, &[(2, 10)]);
    builder.root(0x203);
    builder.object_array(0x203, 0x101, &[0x201]);
    builder.instance(0x201, 0x100, &42i32.to_be_bytes());
    builder.primitive_array(0x202, 8, b"hello");

    let snapshot = snapshot(&builder.finish(), "heap-four");

    let main = snapshot
        .classes
        .iter()
        .find(|class| class.name == "demo.Main")
        .expect("the class of the program");
    assert_eq!((main.count, main.shallow, main.retained), (1, 4, 4));
    assert_eq!(main.package, "demo");
    let array = snapshot
        .classes
        .iter()
        .find(|class| class.name == "demo.Main[]")
        .expect("the array class");
    assert_eq!(array.retained, 28, "the array retains its element");
    assert_eq!(snapshot.roots, 1);

    let instance = find(&snapshot, 0x201);
    let object = &snapshot.objects[instance as usize];
    assert_eq!(object.fields.len(), 1);
    assert_eq!(object.fields[0].name.as_ref(), "value");
    assert_eq!(object.fields[0].value, HeapValue::Number(42));
    let path = snapshot.path(instance);
    assert_eq!(path.len(), 2, "the root reaches the instance in one step");
    assert_eq!(path[1], instance);
    assert!(snapshot.objects[path[0] as usize].root);

    let array = find(&snapshot, 0x202);
    assert_eq!(
        snapshot.objects[array as usize].fields[0].value,
        HeapValue::Array("104, 101, 108, 108, 111".to_owned())
    );
}

#[test]
fn every_kind_of_root_marks_its_object_and_the_stream_stays_in_step() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.load_class(0x100, 1);
    builder.class_dump(0x100, 0, 0, &[]);
    builder.root_jni_global(0x201, 0x900);
    builder.root_java_frame(0x202);
    builder.root_sticky_class(0x203);
    builder.root(0x204);
    builder.instance(0x201, 0x100, &[]);
    builder.instance(0x202, 0x100, &[]);
    builder.instance(0x203, 0x100, &[]);
    builder.instance(0x204, 0x100, &[]);

    let snapshot = snapshot(&builder.finish(), "heap-roots");
    assert_eq!(snapshot.roots, 4);
    for address in [0x201, 0x202, 0x203, 0x204] {
        let object = find(&snapshot, address);
        assert!(
            snapshot.objects[object as usize].root,
            "{address:#x} is a root"
        );
        assert_eq!(
            snapshot.class_of(object).map(|class| class.name.as_str()),
            Some("demo.Main")
        );
    }
}

#[test]
fn the_graph_follows_every_element_of_a_large_array() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.string(2, "[Ldemo/Main;");
    builder.load_class(0x100, 1);
    builder.load_class(0x101, 2);
    builder.class_dump(0x100, 0, 8, &[]);
    let elements: Vec<u64> = (0..20).map(|index| 0x200 + index).collect();
    builder.root(0x500);
    builder.object_array(0x500, 0x101, &elements);
    for element in &elements {
        builder.instance(*element, 0x100, &[]);
    }

    let snapshot = snapshot(&builder.finish(), "heap-large-array");
    let array = find(&snapshot, 0x500);
    assert_eq!(
        snapshot.objects[array as usize].retained,
        100 + 20 * 8,
        "the array keeps all twenty elements, not only the shown ones"
    );
    let last = find(&snapshot, 0x213);
    assert_eq!(snapshot.objects[last as usize].retained, 8);
    assert_eq!(
        snapshot.path(last),
        vec![array, last],
        "an element past the preview still has a path to the root"
    );
}

#[test]
fn eight_byte_identifiers_read_long_fields_and_strings() {
    let mut builder = Builder::new(8);
    builder.string(1, "java/lang/String");
    builder.string(2, "value");
    builder.string(3, "coder");
    builder.load_class(0x110, 1);
    builder.class_dump(0x110, 0, 5, &[(2, 2), (3, 8)]);
    let mut fields = object_bytes(0x210, 8);
    fields.push(0);
    builder.instance(0x211, 0x110, &fields);
    builder.instance(0x212, 0x110, &fields);
    builder.primitive_array(0x210, 8, b"hello");
    builder.root(0x211);

    let snapshot = snapshot(&builder.finish(), "heap-eight");

    let string = snapshot
        .classes
        .iter()
        .find(|class| class.name == "java.lang.String")
        .unwrap();
    assert_eq!(string.count, 2);
    assert_eq!(string.shallow, 10);
    assert_eq!(snapshot.strings.len(), 1, "two strings of the same text");
    assert_eq!(snapshot.strings[0].text.as_ref(), "hello");
    assert_eq!(snapshot.strings[0].count, 2);
    assert_eq!(snapshot.strings[0].wasted, 5);
    assert_eq!(
        snapshot.objects[find(&snapshot, 0x211) as usize]
            .text
            .as_deref(),
        Some("hello")
    );
}

#[test]
fn a_subclass_instance_has_the_fields_of_its_superclass() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Base");
    builder.string(2, "base");
    builder.string(3, "demo/Sub");
    builder.string(4, "value");
    builder.load_class(0x100, 1);
    builder.load_class(0x101, 3);
    builder.class_dump(0x100, 0, 4, &[(2, 10)]);
    builder.class_dump(0x101, 0x100, 4, &[(4, 10)]);
    let mut bytes = 5i32.to_be_bytes().to_vec();
    bytes.extend_from_slice(&7i32.to_be_bytes());
    builder.instance(0x200, 0x101, &bytes);

    let snapshot = snapshot(&builder.finish(), "heap-inheritance");
    let object = &snapshot.objects[find(&snapshot, 0x200) as usize];
    assert_eq!(object.fields.len(), 2);
    assert_eq!(object.fields[0].name.as_ref(), "value");
    assert_eq!(object.fields[0].value, HeapValue::Number(5));
    assert_eq!(object.fields[1].name.as_ref(), "base");
    assert_eq!(object.fields[1].value, HeapValue::Number(7));
}

#[test]
fn an_instance_that_arrives_before_its_class_dump_still_has_its_fields() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.string(2, "value");
    builder.load_class(0x100, 1);
    builder.instance(0x200, 0x100, &5i32.to_be_bytes());
    builder.class_dump(0x100, 0, 4, &[(2, 10)]);

    let snapshot = snapshot(&builder.finish(), "heap-pending");
    let object = &snapshot.objects[find(&snapshot, 0x200) as usize];
    assert_eq!(object.shallow, 4);
    assert_eq!(object.fields.len(), 1);
    assert_eq!(object.fields[0].value, HeapValue::Number(5));
}

#[test]
fn a_weak_reference_is_not_a_path_to_the_root() {
    let mut builder = Builder::new(4);
    builder.string(1, "java/lang/ref/Reference");
    builder.string(2, "referent");
    builder.string(3, "demo/Weak");
    builder.string(4, "demo/Holder");
    builder.string(5, "ref");
    builder.load_class(0x300, 1);
    builder.load_class(0x301, 3);
    builder.load_class(0x302, 4);
    builder.class_dump(0x300, 0, 4, &[(2, 2)]);
    builder.class_dump(0x301, 0x300, 4, &[]);
    builder.class_dump(0x302, 0, 4, &[(5, 2)]);
    builder.root(0x400);
    builder.instance(0x400, 0x302, &object_bytes(0x401, 4));
    builder.instance(0x401, 0x301, &object_bytes(0x402, 4));
    builder.instance(0x402, 0x300, &object_bytes(0, 4));

    let snapshot = snapshot(&builder.finish(), "heap-weak");

    let target = find(&snapshot, 0x402);
    assert!(
        snapshot.objects[target as usize].parent.is_none(),
        "the only reference to the target is a weak one"
    );
    assert_eq!(snapshot.path(target), vec![target]);
    assert_eq!(
        snapshot.objects[find(&snapshot, 0x401) as usize]
            .fields
            .len(),
        1,
        "the field itself is still shown"
    );
}

#[test]
fn a_cut_file_says_where_it_ends_and_a_foreign_file_says_it_is_no_dump() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.load_class(0x100, 1);
    builder.class_dump(0x100, 0, 4, &[]);
    let bytes = builder.finish();
    let path = test_support::scratch("heap-cut").join("cut.hprof");
    fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
    let error = read(&path, &ProjectModel::default(), &Cancel::new(), &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("heap dump") || error.contains("failed to fill"),
        "{error}"
    );

    fs::write(&path, b"not a heap dump at all").unwrap();
    let error = read(&path, &ProjectModel::default(), &Cancel::new(), &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a heap dump"), "{error}");
}

#[test]
fn dominators_of_a_chain_a_diamond_a_cycle_and_two_roots_are_exact() {
    let cancel = Cancel::new();
    let chain = analyse(&[10, 20, 30], &[0], &[(0, 1), (1, 2)], &cancel).unwrap();
    assert_eq!(chain.retained, [60, 50, 30]);
    assert_eq!(chain.parent, [u32::MAX, 0, 1]);

    let diamond = analyse(
        &[10, 10, 10, 10],
        &[0],
        &[(0, 1), (0, 2), (1, 3), (2, 3)],
        &cancel,
    )
    .unwrap();
    assert_eq!(diamond.retained, [40, 10, 10, 10]);
    assert_eq!(diamond.dominator[3], 0);

    let cycle = analyse(&[10, 10, 10], &[0], &[(0, 1), (1, 2), (2, 1)], &cancel).unwrap();
    assert_eq!(cycle.retained, [30, 20, 10]);
    assert_eq!(cycle.dominator[1], 0);

    let two = analyse(&[10, 10, 10], &[0, 1], &[(0, 2), (1, 2)], &cancel).unwrap();
    assert_eq!(two.retained[2], 10);
    assert_eq!(two.dominator[2], 3, "no single root dominates the target");
    assert_eq!(two.parent[2], 0, "the first root reaches it in one step");
}

#[test]
fn cancellation_stops_a_reading() {
    let mut builder = Builder::new(4);
    builder.string(1, "demo/Main");
    builder.load_class(0x100, 1);
    builder.class_dump(0x100, 0, 4, &[]);
    builder.instance(0x200, 0x100, &[]);
    let path = test_support::scratch("heap-cancel").join("dump.hprof");
    fs::write(&path, builder.finish()).unwrap();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = read(&path, &ProjectModel::default(), &cancel, &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"), "{error}");
}
