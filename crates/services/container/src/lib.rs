//! Docker through its CLI: the daemon, contexts, containers and images, actions on them,
//! live logs for a console and command lines for the built-in terminal. The CLI knows the
//! contexts, the pipe of Docker Desktop and the user's credentials, so nothing of that is
//! repeated here.

mod cli;
mod watch;

pub use cli::{ComposeAction, Container, ContainerState, Context, DaemonState, DockerCli, Image};
pub use watch::{DockerSnapshot, DockerWatch, take};
