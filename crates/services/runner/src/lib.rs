//! Running a project and its tests: stored run configurations, a console for what they
//! print and a live tree of test results. What a configuration means belongs to its
//! provider, the core keeps it as opaque data.

mod compound;
mod config;
mod console;
mod coverage;
mod environment;
mod history;
mod live;
mod profile;
mod services;
mod tasks;
mod testing;

pub use compound::CompoundProvider;
pub use config::{
    AttachPlan, BeforeLaunchStep, COMPOUND_PROVIDER, ConfigField, ConfigFieldKind, ConfigTemplate,
    LaunchPoint, RunConfig, RunConfigProvider, RunPlan, configs_file, format_environment,
    join_arguments, load_configs, merge_configs, parse_environment, save_configs, split_arguments,
};
pub use console::{Console, ConsoleEvent, ConsoleEvents, Step, Stream};
pub use coverage::{CoverageOwner, CoveragePlan, TestCoverageRun};
pub use environment::{parse_environment_file, read_environment_file, with_environment};
pub use history::{
    HISTORY_FILE, HISTORY_LIMIT, TestHistoryRun, load_test_history, save_test_history,
    test_history_file,
};

pub use live::{
    LiveApplication, LiveEndpoint, LiveNote, LiveRequest, LiveRow, LiveSection, LiveTable,
    LiveTarget, LiveTone,
};
pub use profile::{ProfileOwner, ProfilePlan};
pub use services::{ServiceGroup, ServiceRun, ServiceState};
pub use tasks::{BuildTaskProvider, PROVIDER_ID as BUILD_TASK_PROVIDER};
pub use testing::{
    FindTests, STOPPED_BY_USER, TestCounts, TestDebug, TestEvent, TestKind, TestListen, TestNode,
    TestOutcome, TestProfileRun, TestProvider, TestRequest, TestRun, TestSite, TestState, TestTree,
    TreeNode,
};
