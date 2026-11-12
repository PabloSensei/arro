//! What a project consists of, whatever tool builds it: modules, roots, dependencies and
//! outputs. A tool plugs in through [`BuildSystem`], the core never asks which one it is.

mod build_tools;
mod cache;
mod environment;
mod model;
mod open;

pub use build_tools::{BuildToolSettings, BuildTools};
pub use cache::{cache_file, default_cache_root};
pub use environment::{
    EnvironmentDetail, EnvironmentProvider, LanguageLevelInfo, ProjectEnvironmentInfo, RuntimeInfo,
    RuntimeReason,
};
pub use model::{
    BuildGroup, BuildModule, BuildTask, MAIN_UNIT, Module, PathEntry, PathKind, ProjectModel,
    TEST_UNIT, Unit,
};
pub use open::{BuildSystem, ProjectSession, SyncEvent, TaskOptions, open_project};
