//! What the MCP server shows and what it refuses before a client calls a tool: the switches
//! of the Settings page, as the gateway applies them.
//!
//! The debugger tools start as router-only, as in IDEA: a client sees `execute_tool` and
//! reaches them by name. A tool the user turned off is neither listed nor called. The
//! router-only mode of the page leaves `execute_tool` as the only tool of the list, for
//! every client or for the ACP sessions alone.

use std::collections::BTreeMap;

use crate::backend::SessionScope;
use crate::tools::{
    EXECUTE_TOOL, LIST_BREAKPOINTS, REMOVE_BREAKPOINT, SET_BREAKPOINT, TOOLS, ToolSpec,
};

/// Whether a tool is on, and whether only `execute_tool` may call it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolState {
    pub enabled: bool,
    pub router_only: bool,
}

/// The clients the router-only mode leaves with `execute_tool` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouterScope {
    AllAgents,
    AcpOnly,
}

/// The switches of the Exposed Tools table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolExposure {
    states: BTreeMap<String, ToolState>,
    router: Option<RouterScope>,
}

impl ToolExposure {
    /// The state of a tool: what the user chose, or the one it starts with.
    pub fn state(&self, name: &str) -> ToolState {
        self.states
            .get(name)
            .copied()
            .unwrap_or_else(|| default_state(name))
    }

    pub fn set_state(&mut self, name: &str, state: ToolState) {
        self.states.insert(name.to_string(), state);
    }

    /// The scope of the router-only mode, or `None` when the mode is off.
    pub fn set_router_only(&mut self, scope: Option<RouterScope>) {
        self.router = scope;
    }

    pub fn router_only(&self) -> Option<RouterScope> {
        self.router
    }

    /// Whether a client of this session sees the tool in `tools/list`.
    pub fn is_visible(&self, name: &str, session: &SessionScope) -> bool {
        if !self.state(name).enabled {
            return false;
        }
        if self.router_applies(session) {
            return name == EXECUTE_TOOL;
        }
        !self.state(name).router_only
    }

    /// Every tool the client of this session sees, in the order of the server.
    pub fn visible(&self, session: &SessionScope) -> Vec<ToolSpec> {
        TOOLS
            .iter()
            .filter(|spec| self.is_visible(spec.name, session))
            .cloned()
            .collect()
    }

    /// Why a call does not reach the service, when it does not. A call from `execute_tool`
    /// reaches a router-only tool, which is the point of the mode.
    pub fn denial(
        &self,
        name: &str,
        session: &SessionScope,
        via_router: bool,
    ) -> Option<&'static str> {
        if !self.state(name).enabled {
            return Some("the tool is turned off in Settings | Tools | AI | MCP Server");
        }
        if via_router || name == EXECUTE_TOOL {
            return None;
        }
        if self.router_applies(session) {
            return Some("only execute_tool is exposed in router-only mode");
        }
        self.state(name)
            .router_only
            .then_some("the tool is router-only, call it through execute_tool")
    }

    fn router_applies(&self, session: &SessionScope) -> bool {
        match self.router {
            Some(RouterScope::AllAgents) => true,
            Some(RouterScope::AcpOnly) => session.is_acp(),
            None => false,
        }
    }
}

/// The switch a tool starts with: on, and router-only for the tools of the debugger.
pub fn default_state(name: &str) -> ToolState {
    ToolState {
        enabled: true,
        router_only: is_debug_tool(name),
    }
}

/// The tools of the debugger, which the page puts in router-only by default.
pub fn is_debug_tool(name: &str) -> bool {
    name.starts_with("debug_")
        || name.starts_with("xdebug_")
        || matches!(name, SET_BREAKPOINT | REMOVE_BREAKPOINT | LIST_BREAKPOINTS)
}
