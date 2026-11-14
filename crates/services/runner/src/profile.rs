//! The profile side of a run: the plan that starts a recording and the snapshot a
//! provider reads back. The core keeps only the path of the recording and the tree a
//! provider gives it, so no tool of a language appears here.

use std::path::{Path, PathBuf};

use base::Result;
use lang_api::{ProfileRecording, ProfileReport, ProfileSnapshot};
use project_model::ProjectModel;

use crate::{RunPlan, TestProvider};

/// A run whose recorded samples go to `recording`.
pub struct ProfilePlan {
    pub plan: RunPlan,
    /// Where the runtime writes the recording.
    pub recording: std::path::PathBuf,
    /// What the user has to know about this run, such as replaced project settings.
    pub note: Option<String>,
}

/// What a started profile run reports to: the configuration provider or the test
/// provider that made the plan.
#[derive(Clone)]
pub enum ProfileOwner {
    Configuration(std::sync::Arc<dyn crate::RunConfigProvider>),
    Tests(std::sync::Arc<dyn TestProvider>),
}

impl ProfileOwner {
    /// Reads the recording a run left in `recording` into the model of the window.
    pub fn report(&self, recording: &Path, model: &ProjectModel) -> Result<ProfileReport> {
        match self {
            Self::Configuration(provider) => provider.profile_report(recording, model),
            Self::Tests(provider) => provider.profile_report(recording, model),
        }
    }

    /// The CPU tree alone, for a caller that shows nothing else.
    pub fn snapshot(&self, recording: &Path, model: &ProjectModel) -> Result<ProfileSnapshot> {
        self.report(recording, model).map(|report| report.cpu_tree)
    }

    /// Every file the run of `recording` left: a runtime may write one per JVM.
    pub fn run_recordings(&self, recording: &Path) -> Vec<PathBuf> {
        match self {
            Self::Configuration(provider) => provider.profile_run_recordings(recording),
            Self::Tests(provider) => provider.profile_run_recordings(recording),
        }
    }

    /// The recordings the provider can open again, the newest first.
    pub fn recordings(&self, model: &ProjectModel) -> Result<Vec<ProfileRecording>> {
        match self {
            Self::Configuration(provider) => provider.profile_recordings(model),
            Self::Tests(provider) => provider.profile_recordings(model),
        }
    }
}
