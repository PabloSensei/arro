//! The question an MCP tool of the class `Execute` puts to the user. The thread of the
//! server waits in [`Approval::ask`], the panel shows the question as a notification and
//! its buttons answer it. A question nobody answers is refused, as is a call in a project
//! the settings do not allow to run without one.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use store::TrustList;
use tool_gateway::{Approval, ApprovalAnswer, ApprovalRequest};
use workspace::Workspace;

/// How long a question waits for the user before the call is refused.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

struct Pending {
    request: ApprovalRequest,
    /// The place or name the call works on, for the text of the question.
    detail: String,
    answer: Option<ApprovalAnswer>,
}

#[derive(Default)]
struct State {
    pending: Option<Pending>,
}

/// The answer the user gave to a question: the buttons of the notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Once,
    Session,
    Deny,
}

impl Reply {
    fn answer(self) -> ApprovalAnswer {
        match self {
            Self::Once => ApprovalAnswer::Once,
            Self::Session => ApprovalAnswer::Session,
            Self::Deny => ApprovalAnswer::Deny,
        }
    }
}

/// The questions of one window. One at a time: a call that arrives while another question
/// waits is refused rather than replacing it.
pub(crate) struct McpApprovals {
    state: Mutex<State>,
    answered: Condvar,
    /// Whether the settings let a trusted project run without a question.
    allow_trusted: Mutex<bool>,
    /// The project of the window and the list the trust dialog writes.
    project: Option<PathBuf>,
    trust: Option<TrustList>,
    timeout: Duration,
}

impl Default for McpApprovals {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            answered: Condvar::new(),
            allow_trusted: Mutex::new(false),
            project: None,
            trust: None,
            timeout: ANSWER_TIMEOUT,
        }
    }
}

impl McpApprovals {
    /// The questions of the window of `project`.
    pub fn new(state_dir: PathBuf, project: PathBuf) -> Self {
        Self {
            project: Some(project),
            trust: Some(TrustList::new(&state_dir)),
            ..Self::default()
        }
    }

    /// For a test that should not wait two minutes for a question nobody answers.
    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// "Always ask" or "Allow for trusted projects" of the MCP settings page.
    pub fn set_allow_trusted(&self, allow: bool) {
        *self
            .allow_trusted
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = allow;
    }

    /// A trusted project whose settings allow it runs without a question.
    fn trusted_project(&self) -> bool {
        let allow = *self
            .allow_trusted
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !allow {
            return false;
        }
        match (&self.project, &self.trust) {
            (Some(project), Some(trust)) => trust.is_trusted(project),
            _ => false,
        }
    }

    /// The question on screen, when there is one.
    pub fn pending(&self) -> Option<ApprovalRequest> {
        self.lock()
            .pending
            .as_ref()
            .map(|pending| pending.request.clone())
    }

    /// The place or name the question on screen is about.
    pub fn pending_detail(&self) -> Option<String> {
        self.lock()
            .pending
            .as_ref()
            .map(|pending| pending.detail.clone())
    }

    /// Answers the question with Allow or Deny. False when none waits, and the
    /// notification stays away.
    pub fn answer(&self, yes: bool) -> bool {
        self.reply(if yes { Reply::Once } else { Reply::Deny })
    }

    /// Answers "Allow for this session": the session of the request runs without
    /// questions until the window closes.
    pub fn answer_for_session(&self) -> bool {
        self.reply(Reply::Session)
    }

    fn reply(&self, reply: Reply) -> bool {
        let mut state = self.lock();
        let Some(pending) = &mut state.pending else {
            return false;
        };
        if pending.answer.is_some() {
            return false;
        }
        pending.answer = Some(reply.answer());
        self.answered.notify_all();
        true
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Approval for McpApprovals {
    fn ask(&self, request: &ApprovalRequest) -> ApprovalAnswer {
        self.ask_with(request, "")
    }

    fn ask_with(&self, request: &ApprovalRequest, detail: &str) -> ApprovalAnswer {
        if self.trusted_project() {
            return ApprovalAnswer::Once;
        }
        let mut state = self.lock();
        if state.pending.is_some() {
            return ApprovalAnswer::Deny;
        }
        state.pending = Some(Pending {
            request: request.clone(),
            detail: detail.to_string(),
            answer: None,
        });
        let waited = self
            .answered
            .wait_timeout_while(state, self.timeout, |state| {
                state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.answer.is_none())
            });
        let mut state = match waited {
            Ok((state, _)) => state,
            Err(poison) => poison.into_inner().0,
        };
        let answer = state
            .pending
            .as_ref()
            .and_then(|pending| pending.answer)
            .unwrap_or(ApprovalAnswer::NoAnswer);
        state.pending = None;
        answer
    }
}

/// The commands of the buttons of the notification of a question.
pub(crate) fn bind_actions(workspace: &mut Workspace) {
    for (id, title, reply) in [
        ("ai.mcp.allow", "Allow Agent Tool", Reply::Once),
        (
            "ai.mcp.allow_session",
            "Allow Agent Tool for the Session",
            Reply::Session,
        ),
        ("ai.mcp.deny", "Deny Agent Tool", Reply::Deny),
    ] {
        // The buttons of a notification make no sense in Find Action, so the command
        // stays out of its list.
        workspace.register_hidden_command(id, title, move |_, _, cx| {
            // A command runs inside the update of the window that pressed the button or
            // the key; the answer updates the window back, which waits for the end.
            cx.defer(move |cx| {
                let Some(panel) = cx.try_global::<crate::agents::ActiveAgents>() else {
                    return;
                };
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.answer_mcp_question(reply, cx));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tool_gateway::{Approval, ApprovalAnswer, ApprovalRequest};

    use super::McpApprovals;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            session: "mcp".to_string(),
            client: Some("test".to_string()),
            tool: "run".to_string(),
        }
    }

    #[test]
    fn an_answer_reaches_the_waiting_call() {
        let approvals = Arc::new(McpApprovals::default());
        let waiting = approvals.clone();
        let ask = std::thread::spawn(move || waiting.ask(&request()));
        let started = std::time::Instant::now();
        while approvals.pending().is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "no question");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(approvals.answer(true));
        assert_eq!(
            ask.join().expect("the thread of the call"),
            ApprovalAnswer::Once
        );

        assert!(approvals.pending().is_none(), "the question goes away");
        assert!(!approvals.answer(true), "there is nothing left to answer");
    }

    #[test]
    fn only_one_question_waits_at_a_time() {
        let approvals = Arc::new(McpApprovals::default());
        let first = approvals.clone();
        let ask = std::thread::spawn(move || first.ask(&request()));
        let started = std::time::Instant::now();
        while approvals.pending().is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "no question");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            approvals.ask(&request()),
            ApprovalAnswer::Deny,
            "a second question does not replace the one on screen"
        );
        assert!(approvals.answer(false));
        assert_eq!(
            ask.join().expect("the thread of the call"),
            ApprovalAnswer::Deny
        );
    }

    #[test]
    fn a_session_answer_is_remembered_by_the_caller() {
        let approvals = Arc::new(McpApprovals::default());
        let wait = approvals.clone();
        let ask = std::thread::spawn(move || wait.ask_with(&request(), "demo.Vitest"));
        let started = std::time::Instant::now();
        while approvals.pending().is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "no question");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(approvals.pending_detail().as_deref(), Some("demo.Vitest"));
        assert!(approvals.answer_for_session());

        assert_eq!(
            ask.join().expect("the thread of the call"),
            ApprovalAnswer::Session
        );
    }

    #[test]
    fn a_question_nobody_answers_says_so() {
        let approvals = McpApprovals::default().with_timeout(Duration::from_millis(50));

        assert_eq!(approvals.ask(&request()), ApprovalAnswer::NoAnswer);
        assert!(approvals.pending().is_none());
    }

    #[test]
    fn a_trusted_project_runs_without_a_question_only_when_the_settings_allow_it() {
        let state = test_support::TempDir::new("mcp-trusted");
        let project = state.path().join("project");
        store::TrustList::new(state.path()).trust(&project).unwrap();
        let approvals = McpApprovals::new(state.path().to_path_buf(), project)
            .with_timeout(Duration::from_millis(50));

        assert_eq!(
            approvals.ask(&request()),
            ApprovalAnswer::NoAnswer,
            "Always ask is the default"
        );

        approvals.set_allow_trusted(true);
        assert_eq!(approvals.ask(&request()), ApprovalAnswer::Once);
        assert!(
            approvals.pending().is_none(),
            "a trusted project shows no notification"
        );
    }
}
