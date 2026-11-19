//! Running Java programs: what differs by JDK version, the classpath jar, finding `main`,
//! the provider that turns a stored configuration into a command, finding tests and running
//! them through the build tool.

mod attach;
mod coverage;
mod flags;
mod live;
mod mains;
mod pathing;
mod profile;
mod provider;
mod quarkus;
mod test_sources;
mod testing;

pub use attach::{
    DebuggableProcess, debug_attach_processes, debug_attach_processes_in, jdwp_address, parse_jps,
};
pub use coverage::{exec_file, report};
pub use flags::JvmFlags;
pub use live::{
    SpringLive, SpringSettings, parse_beans, parse_environment, parse_health, parse_mappings,
    server_port, started_line,
};
pub use mains::{EntryPoint, find_entry_points, parse_entry_points};
pub use pathing::{classpath_jar, write_classpath_jar};
pub use profile::{recording_file, recording_flag};
pub use provider::{DEBUG_AGENT, JavaProvider, JavaTarget, PROVIDER_ID};
pub use quarkus::QuarkusDevProvider;
pub use test_sources::{TestClass, TestMethod, find_tests, parse_tests};
pub use testing::JavaTestProvider;
