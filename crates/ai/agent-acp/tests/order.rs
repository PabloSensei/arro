//! A turn must come out in the order the agent sent its messages. The stub sends a burst of
//! `session/update` notifications and the answer right after it, so both are ready at the same
//! time: a worker that ends the turn on the answer alone loses or reorders the text.

use std::time::{Duration, Instant};

use agent_acp::{AcpCommand, AcpRuntime};
use agent_core::{AgentEvent, AgentRuntime, AgentSession, SessionSpec, SessionStatus};
use crossbeam_channel::Receiver;

const WAIT: Duration = Duration::from_secs(20);
/// The burst and the answer land together only now and then, so the test takes many turns: a
/// single turn that ends before its text fails the run.
const TURNS: usize = 200;
const BURST: usize = 4;
/// A handshake happens once per process, so every round starts its own stub.
const HANDSHAKES: usize = 32;
/// The chunks the stub sends just before the answer to `session/new`.
const GREETING: usize = 2;

struct Fixture {
    _project: test_support::TempDir,
    session: Box<dyn AgentSession>,
    events: Receiver<AgentEvent>,
}

impl Fixture {
    fn new(name: &str, args: &[&str]) -> Self {
        let project = test_support::TempDir::new(name);
        let args = args.iter().map(|arg| arg.to_string()).collect();
        let stub = AcpCommand::new(env!("CARGO_BIN_EXE_acp-stub"), args);
        let runtime = AcpRuntime::new("stub", stub);
        let session = runtime
            .start(SessionSpec {
                project: project.path().to_path_buf(),
                cwd: project.path().to_path_buf(),
                cols: 80,
                rows: 24,
                tools: None,
            })
            .expect("start");
        let events = session.events();
        Self {
            _project: project,
            session,
            events,
        }
    }

    /// Reads until `done`, returns everything read.
    fn until(&self, done: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let Ok(event) = self.events.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            let stop = done(&event);
            seen.push(event);
            if stop {
                return seen;
            }
        }
        panic!("no end in {seen:?}");
    }

    /// Everything of one turn: the call stops at `TurnEnded`.
    fn turn(&self) -> Vec<AgentEvent> {
        self.until(|event| *event == AgentEvent::TurnEnded)
    }

    fn text(events: &[AgentEvent]) -> String {
        let deltas = events.iter().filter_map(|event| match event {
            AgentEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        });
        deltas.collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.session.close();
    }
}

#[test]
fn a_burst_of_updates_stays_before_the_end_of_its_turn() {
    let fixture = Fixture::new("acp-order-turn", &[]);
    fixture.until(|event| *event == AgentEvent::Status(SessionStatus::WaitingForUser));
    let script = format!("BURST {BURST}");
    let expected: String = (0..BURST).map(|index| index.to_string()).collect();

    for turn in 0..TURNS {
        fixture.session.prompt(&script).expect("prompt");
        let events = fixture.turn();
        // The call stops at `TurnEnded`: a delta the worker handled after it is missing here and
        // shows up in the next turn instead.
        assert_eq!(Fixture::text(&events), expected, "turn {turn}");
    }
}

#[test]
fn a_handshake_ends_after_the_updates_that_came_before_it() {
    let count = GREETING.to_string();
    let args = ["--greet", count.as_str()];

    for round in 0..HANDSHAKES {
        let fixture = Fixture::new("acp-order-wait", &args);
        let events =
            fixture.until(|event| *event == AgentEvent::Status(SessionStatus::WaitingForUser));
        assert_eq!(
            Fixture::text(&events),
            "hello ".repeat(GREETING),
            "handshake {round}"
        );
    }
}
