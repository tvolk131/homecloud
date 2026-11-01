use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationKind {
    T3,
    /// Administrator-only disposable host with its own Incus/ZFS/controller.
    HomecloudHost,
}
impl ApplicationKind {
    pub fn version(self) -> &'static str {
        match self {
            Self::T3 => "0.1.2",
            Self::HomecloudHost => "0.1.0",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedApplication {
    pub id: Uuid,
    pub vm_id: Uuid,
    pub kind: ApplicationKind,
    pub desired_version: String,
    pub start_on_boot: bool,
    pub observation: ApplicationObservation,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplicationObservation {
    /// Allowlisted display metadata. Snapshot APIs expose this only to the VM owner.
    #[serde(default)]
    pub accounts: Vec<ApplicationAccount>,
    pub installed_version: Option<String>,
    #[serde(default)]
    pub components: Vec<ApplicationComponent>,
    pub service: String,
    pub setup: String,
    pub message: String,
    pub observed_at: u64,
    /// Owner-scoped, short-lived browser challenge. Never persist this in the host DB.
    pub action: Option<BrowserAction>,
    #[serde(default)]
    pub operation_message: String,
    #[serde(default)]
    pub operation_id: Option<Uuid>,
    #[serde(default)]
    pub operation_stage: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplicationAccount {
    pub name: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    /// Account handle where the service has one, such as a GitHub login.
    #[serde(default)]
    pub username: Option<String>,
    pub plan: Option<String>,
    pub authentication: Option<String>,
    pub observed_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplicationComponent {
    pub name: String,
    pub version: Option<String>,
    pub ready: bool,
    /// Credential presence as reported by the provider, not a successful paid request.
    #[serde(default)]
    pub account: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserAction {
    pub url: String,
    pub code: String,
    pub expires_at: u64,
    #[serde(default)]
    pub setup_action: ApplicationSetupAction,
    #[serde(default)]
    pub input_required: bool,
}
impl std::fmt::Debug for BrowserAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Browser authorization [redacted]")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationSetupAction {
    #[default]
    ConnectT3,
    SignInCodex,
    SignInClaude,
    /// Guest Git/gh access; separate from any provider-connected GitHub plugin.
    #[serde(rename = "sign_in_github")]
    SignInGitHub,
    /// Removes saved GitHub CLI accounts from the guest. GitHub keeps the token valid.
    #[serde(rename = "sign_out_github")]
    SignOutGitHub,
}
impl ApplicationSetupAction {
    /// The guest component whose own account this action changes.
    pub fn provider_name(self) -> Option<&'static str> {
        match self {
            Self::ConnectT3 => None,
            Self::SignInCodex => Some("Codex CLI"),
            Self::SignInClaude => Some("Claude Code"),
            Self::SignInGitHub | Self::SignOutGitHub => Some("GitHub CLI"),
        }
    }
    /// Account state the component reports once this action has succeeded.
    pub fn expected_account(self) -> Option<&'static str> {
        match self {
            Self::ConnectT3 => None,
            Self::SignOutGitHub => Some("signed_out"),
            Self::SignInCodex | Self::SignInClaude | Self::SignInGitHub => Some("signed_in"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthorizationCode(pub String);
impl std::fmt::Debug for AuthorizationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authorization code [redacted]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationOperationKind {
    Install,
    Setup,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPhase {
    Pending,
    Running,
    WaitingForUser,
    Succeeded,
    Failed,
    Interrupted,
}
impl ApplicationPhase {
    pub fn active(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::WaitingForUser)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingForUser => "waiting_for_user",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationOperation {
    pub id: Uuid,
    pub application_id: Uuid,
    pub actor: Uuid,
    pub kind: ApplicationOperationKind,
    #[serde(default)]
    pub setup_action: ApplicationSetupAction,
    pub phase: ApplicationPhase,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationReport {
    pub observation: ApplicationObservation,
    pub job: Option<ApplicationJob>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationJob {
    pub id: Uuid,
    pub phase: ApplicationPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplicationCommand {
    Inspect {
        operation: Option<ApplicationOperation>,
    },
    EnsureInstalled {
        operation: ApplicationOperation,
    },
    StartSetup {
        operation: ApplicationOperation,
    },
    SubmitCode {
        operation: ApplicationOperation,
        code: AuthorizationCode,
    },
}
