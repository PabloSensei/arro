//! Run configurations as the Services window lists them: the group a provider puts a
//! configuration under, and the state of the last run of a configuration. The window
//! feeds the events of the console and the lines the program prints; this module only
//! keeps the state.

/// The group of the Services window one configuration belongs to. A provider that does
/// not show its configurations there says so with `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceGroup {
    /// The key the window keeps the collapsed state and the selection by. It is unique
    /// among the groups of every provider of the project.
    pub id: &'static str,
    /// The name of the group in the tree.
    pub title: &'static str,
    /// The group lists its configurations only after the user chooses Show all run
    /// configurations in its menu: a project has many plain programs and a few services
    /// the user opens the window for.
    pub optional: bool,
}

/// The state of the run of one configuration, as the Services window shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ServiceState {
    #[default]
    NotStarted,
    /// The build before the program or the plan of the run is going.
    Starting,
    /// The program runs; `port` is the one its console named, when it named one.
    Running { port: Option<u16> },
    /// The program ended: `code` is its exit code, `None` when the process said nothing.
    Stopped { code: Option<i32> },
}

impl ServiceState {
    /// The text next to a configuration in the tree: `Running :8080`, `Stopped (1)`.
    pub fn text(&self) -> String {
        match self {
            Self::NotStarted => "Not Started".to_owned(),
            Self::Starting => "Starting".to_owned(),
            Self::Running { port: Some(port) } => format!("Running :{port}"),
            Self::Running { port: None } => "Running".to_owned(),
            Self::Stopped { code: Some(code) } => format!("Stopped ({code})"),
            Self::Stopped { code: None } => "Stopped".to_owned(),
        }
    }

    /// A configuration whose program runs or is about to run.
    pub fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running { .. })
    }

    /// The port a running program listens on.
    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Running { port } => *port,
            _ => None,
        }
    }

    /// The order the tree shows the states in: a configuration that runs goes before one
    /// that never started, as IDEA does.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Starting | Self::Running { .. } => 0,
            Self::NotStarted => 1,
            Self::Stopped { .. } => 2,
        }
    }
}

/// The state of the run of one configuration. The window feeds the events of the console
/// and the port line of the program; every step answers whether the state changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceRun {
    state: ServiceState,
}

impl ServiceRun {
    pub fn state(&self) -> &ServiceState {
        &self.state
    }

    /// The run is planned or the build before the program is going.
    pub fn starting(&mut self) -> bool {
        self.move_to(ServiceState::Starting)
    }

    /// The program itself started.
    pub fn running(&mut self, port: Option<u16>) -> bool {
        self.move_to(ServiceState::Running { port })
    }

    /// A line of the console named the port of the program. Only a running program has a
    /// port to name.
    pub fn port(&mut self, port: u16) -> bool {
        if !matches!(self.state, ServiceState::Running { .. }) {
            return false;
        }
        self.move_to(ServiceState::Running { port: Some(port) })
    }

    /// The program ended with `code`.
    pub fn exited(&mut self, code: Option<i32>) -> bool {
        self.move_to(ServiceState::Stopped { code })
    }

    /// The user stopped the program, so it has no exit code of its own.
    pub fn stopped(&mut self) -> bool {
        self.move_to(ServiceState::Stopped { code: None })
    }

    fn move_to(&mut self, state: ServiceState) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{ServiceRun, ServiceState};

    #[test]
    fn the_state_follows_the_events_of_a_run() {
        let mut run = ServiceRun::default();
        assert_eq!(run.state().text(), "Not Started");
        assert!(!run.state().active());

        assert!(run.starting());
        assert_eq!(run.state().text(), "Starting");
        assert!(run.state().active());

        assert!(run.running(None));
        assert_eq!(run.state().text(), "Running");
        assert_eq!(run.state().port(), None);

        assert!(run.port(8080));
        assert_eq!(run.state().text(), "Running :8080");
        assert_eq!(run.state().port(), Some(8080));
        assert!(!run.port(8080), "the same port changes nothing");

        assert!(run.exited(Some(1)));
        assert_eq!(run.state().text(), "Stopped (1)");
        assert_eq!(run.state().port(), None);
        assert!(!run.state().active());
    }

    #[test]
    fn a_port_of_a_line_belongs_to_a_running_program_only() {
        let mut run = ServiceRun::default();
        assert!(!run.port(8080), "nothing runs yet");
        assert_eq!(run.state(), &ServiceState::NotStarted);

        run.starting();
        assert!(!run.port(8080), "the program has not started");
        assert_eq!(run.state(), &ServiceState::Starting);

        run.running(None);
        assert!(run.port(8080));
        run.stopped();
        assert_eq!(run.state(), &ServiceState::Stopped { code: None });
    }

    #[test]
    fn a_running_configuration_goes_before_the_others() {
        assert!(ServiceState::Running { port: None }.rank() < ServiceState::NotStarted.rank());
        assert!(ServiceState::Starting.rank() < ServiceState::NotStarted.rank());
        assert!(ServiceState::NotStarted.rank() < ServiceState::Stopped { code: Some(0) }.rank());
    }
}
