//! Running a project and its tests: stored run configurations, a console for what they
//! print and a live tree of test results. What a configuration means belongs to its
//! provider, the core keeps it as opaque data.

mod config;
mod console;
mod tasks;
mod testing;

pub use config::{
    ConfigField, ConfigFieldKind, ConfigTemplate, LaunchPoint, RunConfig, RunConfigProvider,
    RunPlan, configs_file, format_environment, join_arguments, load_configs, merge_configs,
    parse_environment, save_configs, split_arguments,
};
pub use console::{Console, ConsoleEvent, ConsoleEvents, Step, Stream};
pub use tasks::{BuildTaskProvider, PROVIDER_ID as BUILD_TASK_PROVIDER};
pub use testing::{
    FindTests, STOPPED_BY_USER, TestCounts, TestDebug, TestEvent, TestKind, TestListen, TestNode,
    TestOutcome, TestProvider, TestRequest, TestRun, TestSite, TestState, TestTree, TreeNode,
};
