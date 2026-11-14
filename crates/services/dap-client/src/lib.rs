//! A debug adapter protocol client and the model of one debugging session.
//!
//! The crate knows the protocol and nothing about the language of the debuggee: the
//! launch arguments and the adapter-specific requests belong to the caller.

mod client;
mod framing;
mod model;
mod session;
mod threads;

pub use client::{Client, Error, Event, REQUEST_TIMEOUT, ResponseError};
pub use model::{
    Breakpoint, BreakpointKind, BreakpointSpec, Capabilities, Command, ConsoleLine, DataAccessType,
    DataBreakpointSpec, DebugState, Debugger, Evaluation, ExceptionFilter, Frame,
    FunctionBreakpointSpec, OutputLine, ReloadOutcome, Scope, Snapshot, StepTarget, Thread,
    Variable, VariableError, WatchValue, line_of, source_path,
};
pub use session::{Launch, normalise, output_sender, run, run_with_output};
pub use threads::{ThreadList, VIRTUAL_PAGE, VirtualThreads, is_virtual_thread, thread_list};

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use proc_sup::ProcessSpec;

/// Marks one debug session for the language side. Dropping it lets the language turn
/// back on whatever it suspended for the session, such as a compiler of its own.
pub trait DebugSessionToken: Send {}

/// Starts a debuggee prepared by the caller and attaches to it. The runner builds the
/// command; the language side knows how the debuggee announces its debug port.
pub trait DebugService: Send + Sync + 'static {
    /// Returns at once. The program starts and the adapter is reached in the background;
    /// failures arrive as a terminated snapshot with the reason in its console.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &self,
        program: ProcessSpec,
        name: String,
        entry: Option<std::path::PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String>;

    /// Attaches to a debuggee that somebody else started and keeps running.
    #[allow(clippy::too_many_arguments)]
    fn attach(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String>;

    /// Attaches to a debuggee that is known to listen already, without probing its port.
    /// The caller took the readiness from the output of the process it started.
    #[allow(clippy::too_many_arguments)]
    fn attach_ready(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.attach(
            host,
            port,
            name,
            project,
            breakpoints,
            watch,
            exception_filters,
        )
    }

    /// Like [`DebugService::start`], with the method breakpoints and field watchpoints
    /// the window already keeps. A language that can send them before the program runs
    /// overrides this; the default sends them right after the session starts.
    #[allow(clippy::too_many_arguments)]
    fn start_with_breakpoints(
        &self,
        program: ProcessSpec,
        name: String,
        entry: Option<std::path::PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        function_breakpoints: Vec<FunctionBreakpointSpec>,
        data_breakpoints: Vec<DataBreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let debugger = self.start(program, name, entry, breakpoints, watch, exception_filters)?;
        let _ = debugger.send(Command::SetFunctionBreakpoints(function_breakpoints));
        let _ = debugger.send(Command::SetDataBreakpoints(data_breakpoints));
        Ok(debugger)
    }

    /// Like [`DebugService::attach`], with the method breakpoints and field watchpoints.
    #[allow(clippy::too_many_arguments)]
    fn attach_with_breakpoints(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        function_breakpoints: Vec<FunctionBreakpointSpec>,
        data_breakpoints: Vec<DataBreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let debugger = self.attach(
            host,
            port,
            name,
            project,
            breakpoints,
            watch,
            exception_filters,
        )?;
        let _ = debugger.send(Command::SetFunctionBreakpoints(function_breakpoints));
        let _ = debugger.send(Command::SetDataBreakpoints(data_breakpoints));
        Ok(debugger)
    }

    /// Like [`DebugService::attach_ready`], with the method breakpoints and field watchpoints.
    #[allow(clippy::too_many_arguments)]
    fn attach_ready_with_breakpoints(
        &self,
        host: String,
        port: u16,
        name: String,
        project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        function_breakpoints: Vec<FunctionBreakpointSpec>,
        data_breakpoints: Vec<DataBreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let debugger = self.attach_ready(
            host,
            port,
            name,
            project,
            breakpoints,
            watch,
            exception_filters,
        )?;
        let _ = debugger.send(Command::SetFunctionBreakpoints(function_breakpoints));
        let _ = debugger.send(Command::SetDataBreakpoints(data_breakpoints));
        Ok(debugger)
    }

    /// Called when a debuggee this process launches starts. The token ends the session
    /// when it is dropped, and the language may keep compilation off for as long as one
    /// is alive. `None` when the language has nothing to suspend.
    fn begin_session(&self) -> Option<Box<dyn DebugSessionToken>> {
        None
    }

    /// Replaces the classes under `outputs` in the running program with the bytes on
    /// disk. `done` receives the answer on a worker thread. False when the language
    /// cannot replace classes; `done` is then not called.
    fn reload_classes(
        &self,
        debugger: Debugger,
        outputs: Vec<PathBuf>,
        done: Sender<ReloadOutcome>,
    ) -> bool {
        let _ = (debugger, outputs, done);
        false
    }
}
