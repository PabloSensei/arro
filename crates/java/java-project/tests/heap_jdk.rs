//! A heap dump of a real program: the JDK of the corpus runs a small class, `jcmd` writes
//! its heap and the reader finds the class and the instances the program made.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base::Cancel;
use java_project::heap::{capture, read};
use java_project::{BuildEnv, Jdk};
use lang_api::HeapValue;
use project_model::ProjectModel;

const PROGRAM: &str = r#"
package demo;

public class Sample {
    static Object keep;

    static class Base {
        int base;
    }

    static class Holder extends Base {
        int value;
    }

    public static void main(String[] args) throws Exception {
        Holder[] holders = new Holder[123];
        for (int index = 0; index < holders.length; index++) {
            holders[index] = new Holder();
            holders[index].value = index;
            holders[index].base = index + 1000;
        }
        keep = holders;
        System.out.println("ready");
        Thread.sleep(20000);
    }
}
"#;

fn corpus_jdk() -> PathBuf {
    let root = std::env::var("IDE_CORPUS")
        .map(PathBuf::from)
        .expect("the corpus is not set: IDE_CORPUS");
    root.join("jdk").join("21")
}

#[test]
#[ignore = "needs the JDK of the corpus at IDE_CORPUS/jdk/21 and starts a program"]
fn a_dump_of_a_running_program_has_the_class_of_the_program_and_its_instances() {
    let jdk_home = corpus_jdk();
    let jdk = Jdk::at(&jdk_home).expect("JDK 21 of the corpus");
    let folder = test_support::TempDir::new("heap-jdk");
    let source = folder.path().join("Sample.java");
    fs::write(&source, PROGRAM).unwrap();
    let classes = folder.path().join("classes");
    fs::create_dir_all(&classes).unwrap();
    let status = Command::new(jdk_home.join("bin").join("javac"))
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .status()
        .expect("javac of the corpus");
    assert!(status.success(), "the program compiles");

    let mut child = Command::new(jdk_home.join("bin").join("java"))
        .arg("-cp")
        .arg(&classes)
        .arg("demo.Sample")
        .stdout(Stdio::piped())
        .spawn()
        .expect("the program starts");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("the output of the program"))
        .read_line(&mut ready)
        .expect("the first line of the program");
    assert_eq!(ready.trim(), "ready", "the program is up");

    let dump = folder.path().join("heap.hprof");
    let env = BuildEnv {
        jdk: Some(jdk.clone()),
        ..BuildEnv::default()
    };
    let model = ProjectModel {
        root: folder.path().to_path_buf(),
        ..ProjectModel::default()
    };
    capture(&env, &jdk, child.id(), &dump, &model, &Cancel::new()).expect("jcmd writes the heap");
    let snapshot = read(&dump, &model, &Cancel::new(), &mut |_| {}).unwrap();
    let _ = child.kill();
    let _ = child.wait();

    let holders = snapshot
        .classes
        .iter()
        .find(|class| class.name == "demo.Sample$Holder")
        .expect("the class of the program in the histogram");
    assert_eq!(holders.count, 123, "one instance for each holder");
    let mut seen = 0;
    for (index, object) in snapshot.objects.iter().enumerate() {
        if snapshot
            .class_of(index as u32)
            .map(|class| class.name.as_str())
            != Some("demo.Sample$Holder")
        {
            continue;
        }
        let number = |name: &str| {
            object
                .fields
                .iter()
                .find(|field| &*field.name == name)
                .and_then(|field| match field.value {
                    HeapValue::Number(value) => Some(value),
                    _ => None,
                })
        };
        assert_eq!(
            number("base"),
            number("value").map(|value| value + 1000),
            "an instance keeps the field of its superclass"
        );
        seen += 1;
    }
    assert_eq!(seen, 123);
    assert!(snapshot.total_bytes > 0);
    assert!(snapshot.roots > 0);
}
