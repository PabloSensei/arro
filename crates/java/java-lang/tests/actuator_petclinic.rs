//! The live sections of the PetClinic of the corpus. Starting the application needs a
//! Maven build and a free port, which is too heavy for a test that runs next to the others,
//! so this test checks what the corpus itself gives: the actuator dependency, the
//! settings file and the paths of the sections. The window test with a fake server covers
//! the readiness line and the data.
//!
//! Run with `IDE_CORPUS=<folder that holds src/petclinic> cargo test -p java-lang --test
//! actuator_petclinic -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};

use java_exec::{JavaTarget, SpringLive};
use project_model::{Module, ProjectModel};
use runner::{LiveApplication, LiveSection};

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
    candidates
        .into_iter()
        .find(|root| root.join("pom.xml").exists())
        .unwrap_or_else(|| {
            panic!("PetClinic is missing; point IDE_CORPUS at the folder that holds src/petclinic")
        })
}

#[test]
#[ignore = "needs the PetClinic corpus: run with --ignored, IDE_CORPUS sets its folder"]
fn petclinic_has_actuator_and_the_default_section_paths() {
    let root = petclinic();
    let pom = fs::read_to_string(root.join("pom.xml")).unwrap();
    assert!(
        pom.contains("spring-boot-starter-actuator"),
        "PetClinic brings the actuator starter"
    );
    let properties =
        fs::read_to_string(root.join("src/main/resources/application.properties")).unwrap();
    assert!(
        properties.contains("management.endpoints.web.exposure.include"),
        "the corpus exposes the endpoints"
    );

    let model = ProjectModel {
        root: root.clone(),
        tool: "maven".to_owned(),
        modules: vec![Module {
            id: "app".to_owned(),
            name: "app".to_owned(),
            dir: root,
            ..Module::default()
        }],
        resolved: false,
        ..ProjectModel::default()
    };
    let target = JavaTarget {
        module: "app".to_owned(),
        main_class: "org.springframework.samples.petclinic.PetClinicApplication".to_owned(),
        spring_boot: true,
        ..JavaTarget::default()
    };
    let live = SpringLive::of(&target, &model).expect("a Spring Boot configuration");
    assert_eq!(
        live.section_path(LiveSection::Health).as_deref(),
        Some("/actuator/health")
    );
    assert_eq!(
        live.section_path(LiveSection::Beans).as_deref(),
        Some("/actuator/beans")
    );
    assert!(!live.started());
    assert!(live.observe("Tomcat started on port 8080 (http)"));
    assert!(live.observe("Started PetClinicApplication in 2.1 seconds"));
    assert!(live.started());
}
