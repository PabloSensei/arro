//! Running Java programs: what differs by JDK version, the classpath jar, finding `main`,
//! the provider that turns a stored configuration into a command, finding tests and running
//! them through the build tool.

mod flags;
mod mains;
mod pathing;
mod provider;
mod test_sources;
mod testing;

pub use flags::JvmFlags;
pub use mains::{EntryPoint, find_entry_points, parse_entry_points};
pub use pathing::{classpath_jar, write_classpath_jar};
pub use provider::{DEBUG_AGENT, JavaProvider, JavaTarget, PROVIDER_ID};
pub use test_sources::{TestClass, TestMethod, find_tests, parse_tests};
pub use testing::JavaTestProvider;
