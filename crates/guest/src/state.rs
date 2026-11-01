use std::path::{Path, PathBuf};

use homecloud_core::{
    ApplicationOperationKind, ApplicationPhase, ApplicationSetupAction, BrowserAction,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;
use crate::fsutil::save_json;

fn preparing() -> String {
    "preparing".into()
}

/// The durable record of one supervised job, private to the guest owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobState {
    pub id: Uuid,
    pub kind: ApplicationOperationKind,
    pub phase: ApplicationPhase,
    pub unit: String,
    pub boot_id: String,
    #[serde(default)]
    pub setup_action: ApplicationSetupAction,
    #[serde(default = "preparing")]
    pub stage: String,
    /// The public browser challenge while waiting for the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<BrowserAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<f64>,
}

impl JobState {
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            ApplicationPhase::Running | ApplicationPhase::WaitingForUser
        )
    }
}

/// A job's state together with the file it is saved to.
pub struct Job {
    pub state: JobState,
    path: PathBuf,
    /// Every saved state, so tests can check what was published.
    #[cfg(test)]
    pub history: Vec<JobState>,
}

impl Job {
    pub fn new(path: PathBuf, state: JobState) -> Self {
        Self {
            state,
            path,
            #[cfg(test)]
            history: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn directory(&self) -> &Path {
        self.path.parent().expect("job file has a directory")
    }

    pub fn save(&mut self) -> Result<()> {
        save_json(&self.path, &self.state)?;
        #[cfg(test)]
        self.history.push(self.state.clone());
        Ok(())
    }

    /// Publishes a challenge for the owner to approve.
    pub fn awaiting(&mut self, action: BrowserAction) -> Result<()> {
        self.state.phase = ApplicationPhase::WaitingForUser;
        self.state.stage = "awaiting_approval".into();
        self.state.action = Some(action);
        self.save()
    }

    /// The owner acted; the challenge is withdrawn while the result is checked.
    pub fn verifying(&mut self) -> Result<()> {
        self.state.phase = ApplicationPhase::Running;
        self.state.stage = "verifying".into();
        self.state.action = None;
        self.save()
    }
}

#[cfg(test)]
pub fn test_job(dir: &Path, action: ApplicationSetupAction) -> Job {
    Job::new(
        dir.join("job.json"),
        JobState {
            id: Uuid::from_u128(2),
            kind: ApplicationOperationKind::Setup,
            phase: ApplicationPhase::Running,
            unit: "fixture.service".into(),
            boot_id: "fixture-boot".into(),
            setup_action: action,
            stage: preparing(),
            action: None,
            message: None,
            completed_at: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_written_jobs_keep_their_challenge_and_optional_defaults() {
        let state: JobState = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-4000-8000-000000000002", "kind":"setup",
            "phase":"waiting_for_user", "unit":"fixture.service", "boot_id":"old-python-boot",
            "setup_action":"sign_in_claude", "completed_at":null,
            "action":{"url":"https://claude.ai/oauth/authorize", "code":"", "expires_at":9999999999u64,
                "setup_action":"sign_in_claude", "input_required":true}
        }))
        .unwrap();
        assert!(state.active());
        assert_eq!(state.stage, "preparing");
        assert_eq!(state.setup_action, ApplicationSetupAction::SignInClaude);
        assert!(state.action.unwrap().input_required);
        let completed: JobState = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-4000-8000-000000000002", "kind":"install", "phase":"succeeded",
            "unit":"fixture.service", "boot_id":"old-python-boot", "completed_at":1234
        }))
        .unwrap();
        assert_eq!(completed.completed_at, Some(1234.0));
        assert_eq!(completed.setup_action, ApplicationSetupAction::ConnectT3);
    }
}
