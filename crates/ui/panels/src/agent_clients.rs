//! The Agents tab of the AI sessions window: who is connected to the MCP server of the
//! project, what each client called, the permissions of its session and the buttons that
//! take them back or close the connection.

use tool_gateway::{CallRecord, ClientRecord, ToolClass};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{AI_CLIENTS, AI_TOOL, Workspace};

use crate::agents::{ActiveAgents, AgentsPanel, Mcp, Tab};
use crate::chat::text_button;

impl AgentsPanel {
    /// The server of the project, when it is up.
    fn mcp(&self) -> Option<&std::sync::Arc<Mcp>> {
        self.services
            .as_ref()
            .map(|services| &services.mcp)
            .or(self.mcp.as_ref())
    }

    /// The MCP clients of the project, in the order they connected.
    pub(crate) fn clients(&self) -> Vec<ClientRecord> {
        self.gateway()
            .map(|gateway| gateway.clients().clients())
            .unwrap_or_default()
    }

    /// The clients that are connected now.
    pub(crate) fn agents_count(&self) -> usize {
        self.gateway()
            .map(|gateway| gateway.clients().connected())
            .unwrap_or(0)
    }

    pub(crate) fn calls_of(&self, client: &ClientRecord) -> Vec<CallRecord> {
        self.gateway()
            .map(|gateway| gateway.clients().calls(&client.session, &client.name))
            .unwrap_or_default()
    }

    /// The gateway of the project, when the MCP server is up and shared.
    fn gateway(&self) -> Option<&std::sync::Arc<tool_gateway::ToolGateway>> {
        Some(&self.mcp()?.gateway)
    }

    fn selected_client(&self) -> Option<ClientRecord> {
        let clients = self.clients();
        let index = self.clients_selected.min(clients.len().saturating_sub(1));
        clients.get(index).cloned()
    }

    /// Shows the tab from the indicator of the status bar and from the command.
    pub(crate) fn show_clients(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Agents;
        cx.notify();
    }

    fn select_client(&mut self, index: usize, cx: &mut Context<Self>) {
        self.clients_selected = index;
        cx.notify();
    }

    /// Takes back "Allow for this session" of the selected client: its next `Execute`
    /// call asks the user again.
    pub(crate) fn revoke_selected(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.selected_client() else {
            return;
        };
        let Some(gateway) = self.gateway() else {
            return;
        };
        gateway.revoke_session(&client.session);
        cx.notify();
    }

    /// Closes the transport session of the selected client on the server. The client
    /// leaves the list as disconnected, while its calls stay on the screen.
    pub(crate) fn disconnect_selected(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.selected_client() else {
            return;
        };
        let Some(gateway) = self.gateway().cloned() else {
            return;
        };
        let connection = gateway.clients().disconnect(
            &client.session,
            &tool_gateway::ClientInfo {
                name: client.name.clone(),
                version: client.version.clone(),
            },
        );
        if let (Some(connection), Some(mcp)) = (connection, self.mcp()) {
            mcp.server.disconnect(&connection);
        }
        cx.notify();
    }

    pub(crate) fn clients_view(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let clients = self.clients();
        let selected = self.clients_selected.min(clients.len().saturating_sub(1));
        let calls = clients
            .get(selected)
            .map(|client| self.calls_of(client))
            .unwrap_or_default();
        let allowed = clients.get(selected).is_some_and(|client| {
            self.gateway()
                .is_some_and(|g| g.session_allowed(&client.session))
        });
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(SPACE_S))
            .text_size(px(TEXT_SMALL));
        content = content.child(
            div()
                .text_color(theme.text_secondary)
                .child("MCP clients of this project"),
        );
        if clients.is_empty() {
            content = content.child(
                div()
                    .text_color(theme.text_secondary)
                    .child("No agent is connected"),
            );
        }
        for (index, client) in clients.iter().enumerate() {
            let chosen = index == selected;
            let (hover, selected_bg) = (theme.hover, theme.selection_inactive);
            let status = if client.connected {
                "Connected"
            } else {
                "Disconnected"
            };
            let status_color = if client.connected {
                theme.accent
            } else {
                theme.text_secondary
            };
            let title = title_of(client);
            let calls = call_count(client.calls);
            content = content.child(
                div()
                    .id(("agent-client", index))
                    .debug_selector(move || {
                        if index == 0 {
                            "agent-client-first".to_owned()
                        } else {
                            format!("agent-client-{index}")
                        }
                    })
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(ROW_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .when(chosen, |this| this.bg(selected_bg))
                    .when(!chosen, |this| this.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_client(index, cx)))
                    .child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .flex_none()
                            .bg(status_color),
                    )
                    .child(div().flex_1().truncate().child(title))
                    .child(div().text_color(theme.text_secondary).child(format!(
                        "{} at {}",
                        calls,
                        clock(client.connected_at_ms)
                    )))
                    .child(div().text_color(status_color).child(status)),
            );
        }
        if let Some(client) = clients.get(selected) {
            let mut permissions = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SPACE_S))
                .pt(px(SPACE_S));
            permissions = permissions.child(
                div()
                    .text_color(if allowed {
                        theme.accent
                    } else {
                        theme.text_secondary
                    })
                    .child(if allowed {
                        "Allowed for this session"
                    } else {
                        "Asks before every launch or edit"
                    }),
            );
            if allowed {
                permissions = permissions.child(
                    text_button("agent-revoke", "Revoke", theme)
                        .debug_selector(|| "agent-revoke".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.revoke_selected(cx))),
                );
            }
            if client.connected {
                permissions = permissions.child(
                    text_button("agent-disconnect", "Disconnect", theme)
                        .debug_selector(|| "agent-disconnect".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect_selected(cx))),
                );
            }
            content = content.child(permissions);
            content = content.child(
                div()
                    .id("agent-calls-only-actions")
                    .debug_selector(|| "agent-calls-only-actions".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .pt(px(SPACE_S))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clients_only_actions = !this.clients_only_actions;
                        cx.notify();
                    }))
                    .child(checkbox(self.clients_only_actions, theme))
                    .child("Only launch and edits"),
            );
            let shown: Vec<&CallRecord> = calls
                .iter()
                .filter(|call| !self.clients_only_actions || call.class == ToolClass::Execute)
                .collect();
            if shown.is_empty() {
                content = content.child(
                    div()
                        .text_color(theme.text_secondary)
                        .child("No calls of this client yet"),
                );
            }
            for (index, call) in shown.into_iter().enumerate() {
                let color = match call.outcome {
                    tool_gateway::CallOutcome::Ok => theme.text_secondary,
                    tool_gateway::CallOutcome::Error => theme.close_hover,
                    tool_gateway::CallOutcome::Refused => theme.warning_mark,
                };
                let arguments = if call.arguments.is_empty() {
                    "-".to_string()
                } else {
                    call.arguments.clone()
                };
                content = content.child(
                    div()
                        .id(("agent-call", call.id))
                        .debug_selector(move || {
                            if index == 0 {
                                "agent-call-first".to_owned()
                            } else {
                                format!("agent-call-{}", call.id)
                            }
                        })
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(ROW_HEIGHT))
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(clock(call.time_ms)),
                        )
                        .child(div().flex_none().child(call.tool.clone()))
                        .child(div().flex_1().truncate().child(arguments))
                        .child(div().text_color(color).child(call.outcome.name()))
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(format!("{} ms", call.duration_ms)),
                        ),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(scrollable(
                "agent-clients",
                &self.clients_scroll,
                content,
                theme,
                window,
                cx,
            ))
            .into_any_element()
    }
}

fn title_of(client: &ClientRecord) -> String {
    if client.version.is_empty() {
        client.name.clone()
    } else {
        format!("{} {}", client.name, client.version)
    }
}

fn call_count(calls: usize) -> String {
    if calls == 1 {
        "1 call".to_string()
    } else {
        format!("{calls} calls")
    }
}

/// The time of a call or of a connection as the tab shows it, in the time of the machine
/// only up to the day: the journal is read within one session.
fn clock(time_ms: u64) -> String {
    let seconds = time_ms / 1000 % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The command of the agents indicator of the status bar: it shows the AI window on the
/// tab of the connected clients.
pub(crate) fn bind_actions(workspace: &mut Workspace) {
    workspace.register_hidden_command(
        AI_CLIENTS,
        "Show Connected Agents",
        |workspace, window, cx| {
            workspace.show_tool(AI_TOOL, window, cx);
            let Some(panel) = cx.try_global::<ActiveAgents>().map(|panel| panel.0.clone()) else {
                return;
            };
            panel.update(cx, |panel, cx| panel.show_clients(cx));
        },
    );
}
