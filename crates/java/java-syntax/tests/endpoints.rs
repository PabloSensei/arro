//! The HTTP handlers of PetClinic, read by the native scan of the grammar. The project
//! lives in `corpus/` next to the repository, outside git, so the test is ignored by
//! default. Run it with
//! `IDE_CORPUS=<folder that holds src/petclinic> cargo test -p java-syntax --test endpoints -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};

fn petclinic() -> PathBuf {
    let own = std::env::var_os("IDE_CORPUS").map(PathBuf::from);
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates: Vec<PathBuf> = match own {
        Some(folder) => vec![folder.join("src/petclinic")],
        None => manifest
            .ancestors()
            .map(|folder| folder.join("corpus/src/petclinic"))
            .collect(),
    };
    let found = candidates.iter().find(|root| root.join("pom.xml").exists());
    let Some(found) = found else {
        panic!("PetClinic is missing, looked for pom.xml in {candidates:?}");
    };
    fs::canonicalize(found).expect("the corpus folder can be read")
}

fn sources(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
        {
            found.push(path);
        }
    }
}

#[test]
#[ignore = "needs the PetClinic corpus: run with --ignored, IDE_CORPUS sets its folder"]
fn petclinic_handlers_are_found() {
    let root = petclinic();
    let mut files = Vec::new();
    sources(&root.join("src/main/java"), &mut files);
    let mut all = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        all.extend(java_syntax::endpoints(&file, &text));
    }
    let labels: Vec<String> = all
        .iter()
        .map(|endpoint| format!("{} {}", endpoint.method.as_str(), endpoint.path))
        .collect();
    println!("{} handlers", labels.len());
    for label in &labels {
        println!("{label}");
    }
    for wanted in [
        "GET /owners/{ownerId}",
        "POST /owners/new",
        "GET /vets.html",
    ] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "missing {wanted} in {labels:?}"
        );
    }
}
