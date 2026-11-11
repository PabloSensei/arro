use std::sync::Arc;
use std::time::Duration;

use agent_core::{
    AgentError, AgentRuntime, AgentSession, Harness, Installed, RuntimeCaps, SessionSpec,
    SurfaceKind,
};
use terminal::{SpawnSpec, TerminalSession};

use crate::launch::{Launch, launch_arguments, shift_enter};
use crate::session::{FIRST_PROMPT_WAIT, PtySession, Rules};

pub struct PtyRuntime {
    installed: Installed,
    launch: Launch,
    /// Arguments of every run, after those that connect the tools.
    extra_arguments: Vec<String>,
    first_prompt_wait: Duration,
}

impl PtyRuntime {
    pub fn new(installed: Installed, launch: Launch) -> Self {
        Self {
            installed,
            launch,
            extra_arguments: Vec::new(),
            first_prompt_wait: FIRST_PROMPT_WAIT,
        }
    }

    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.extra_arguments = arguments;
        self
    }

    /// How long the first prompt waits for the input box before it is refused.
    pub fn with_first_prompt_wait(mut self, wait: Duration) -> Self {
        self.first_prompt_wait = wait;
        self
    }

    fn accepts_prompts(&self) -> bool {
        self.installed.harness != Harness::OpenCode
    }
}

impl AgentRuntime for PtyRuntime {
    fn name(&self) -> &str {
        self.installed.harness.title()
    }

    fn caps(&self) -> RuntimeCaps {
        RuntimeCaps {
            streaming: false,
            permissions: false,
            resume: false,
            modes: false,
            surface: SurfaceKind::Terminal,
            prompts: self.accepts_prompts(),
        }
    }

    fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, AgentError> {
        let harness = self.installed.harness;
        let mut arguments = launch_arguments(harness, &spec);
        arguments.extend(self.extra_arguments.iter().cloned());
        let terminal = TerminalSession::spawn(&SpawnSpec {
            command_line: self.installed.command_line(&arguments),
            cwd: Some(spec.cwd.clone()),
            env: self.launch.environment(&spec),
            cols: spec.cols,
            rows: spec.rows,
            shift_enter: shift_enter(harness),
        })?;
        platform::label_process(terminal.pid(), "AI harness");
        let rules = Rules {
            harness,
            marker: self.launch.marker().map(str::to_string),
            prompts: self.accepts_prompts(),
            first_prompt_wait: self.first_prompt_wait,
        };
        Ok(Box::new(PtySession::start(Arc::new(terminal), rules)?))
    }
}
