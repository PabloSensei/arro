//! The remote configuration of the Java provider: it starts nothing and gives the
//! debugger the address of a JVM that another process runs.

use std::path::Path;
use std::sync::Arc;

use base::Cancel;
use java_exec::{JavaProvider, JavaTarget};
use java_project::{BuildEnv, Jdk};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::RunConfigProvider;

struct NoTool;

impl BuildSystem for NoTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Err("not used".into())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("assemble"))
    }
}

fn provider(cache: &Path) -> JavaProvider {
    let env = BuildEnv {
        jdk: Some(Jdk {
            home: "C:/jdk".into(),
            version: "17.0.1".into(),
            feature: 17,
        }),
        cache_root: Some(cache.to_owned()),
        ..BuildEnv::default()
    };
    JavaProvider::new(env, Arc::new(NoTool))
}

fn model(root: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_owned(),
        resolved: true,
        ..ProjectModel::default()
    }
}

#[test]
fn the_remote_configuration_gives_the_debugger_its_host_and_port() {
    let root = test_support::scratch("java-exec-remote");
    let provider = provider(&root.join("cache"));
    let model = model(&root);

    assert!(
        provider
            .templates()
            .iter()
            .any(|template| template.id == "remote" && template.name == "Remote JVM Debug"),
        "the Add button offers the remote configuration"
    );
    let config = provider.create_config("remote", &model).unwrap();
    assert_eq!(config.name, "Remote JVM Debug");
    assert!(!config.assemble_first, "there is nothing to build");
    let target: JavaTarget = config.settings.clone().try_into().unwrap();
    assert!(target.remote);
    assert_eq!(target.host, "localhost");
    assert_eq!(target.port, 5005);

    let attach = provider
        .attach_plan(&config, &model)
        .unwrap()
        .expect("the remote configuration attaches");
    assert_eq!(attach.host, "localhost");
    assert_eq!(attach.port, 5005);
    assert_eq!(attach.name, "Remote JVM Debug");
    assert!(
        provider.plan(&config, &model).is_err(),
        "the remote configuration starts no program"
    );
    assert!(provider.debug_plan(&config, &model).is_err());
}

#[test]
fn the_dialog_shows_the_command_of_the_remote_jvm_and_takes_the_address() {
    let root = test_support::scratch("java-exec-remote-fields");
    let provider = provider(&root.join("cache"));
    let model = model(&root);
    let mut config = provider.create_config("remote", &model).unwrap();

    let keys: Vec<&str> = provider
        .fields(&config)
        .into_iter()
        .map(|field| field.key)
        .collect();
    assert_eq!(keys, vec!["host", "port", "agent"]);
    assert_eq!(provider.read_field(&config, "host"), "localhost");
    assert_eq!(provider.read_field(&config, "port"), "5005");
    assert_eq!(
        provider.read_field(&config, "agent"),
        "-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005"
    );

    provider.write_field(&mut config, "host", "build-box");
    provider.write_field(&mut config, "port", "5006");
    let attach = provider.attach_plan(&config, &model).unwrap().unwrap();
    assert_eq!(attach.host, "build-box");
    assert_eq!(attach.port, 5006);
    assert!(
        provider
            .read_field(&config, "agent")
            .ends_with("address=*:5006")
    );
}
