//! Maven and Gradle projects: finding the tool and the JDK, a quick scan of project files,
//! and the full model the tool reports through the helpers in `helpers/`.

mod env;
mod environment;
mod fingerprint;
mod gradle;
mod jdk;
mod lombok;
mod maven;
mod pom;
mod test_runs;

pub use env::{BuildEnv, detect_build_system};
pub use environment::{
    ProjectSettings, ResolvedEnvironment, language_level, load_settings, project_jdk,
    resolve_environment, resolve_with, runtime_info, save_settings, settings_file,
};
pub use gradle::{Gradle, gradle_supports, wrapper_version};
pub use jdk::{Jdk, Requested, choose_jdk, feature_of, find_jdks, requested_jdk};
pub use lombok::{
    LOMBOK_ARTIFACT, LOMBOK_GROUP, LocalRepositories, Lombok, LombokDependency, find_lombok,
    find_lombok_in_build_files, resolve_lombok,
};
pub use maven::{Maven, find_maven};
pub use pom::{Pom, read_pom, scan_poms};
pub use test_runs::{TEST_LISTENER, TestHook, TestSelection};
