//! Allowlisted account display data. Credentials and raw provider replies
//! never leave the adapters.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::text::display_text;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    SignedIn,
    SignedOut,
    #[default]
    Unknown,
    Unavailable,
}

impl AccountState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignedIn => "signed_in",
            Self::SignedOut => "signed_out",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub state: AccountState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<String>,
}

impl From<AccountState> for AccountInfo {
    fn from(state: AccountState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }
}

/// Codex `account/read` result.
pub fn codex_account(value: &Value) -> AccountInfo {
    let Some(value) = value.as_object() else {
        return AccountState::Unknown.into();
    };
    let account = match value.get("account") {
        None | Some(Value::Null) => return AccountState::SignedOut.into(),
        Some(account) => account,
    };
    let kind = account.get("type").and_then(Value::as_str);
    let Some(kind @ ("chatgpt" | "apiKey")) = kind else {
        return AccountState::Unknown.into();
    };
    let routing = value.get("workspaceRouting").filter(|r| r.is_object());
    AccountInfo {
        state: AccountState::SignedIn,
        email: display_text(account.get("email"), 320),
        account_id: display_text(routing.and_then(|r| r.get("chatgptAccountId")), 128),
        username: None,
        plan: display_text(account.get("planType"), 80),
        authentication: Some(kind.to_owned()),
    }
}

/// `claude auth status` output and exit code.
pub fn claude_account(value: &Value, code: Option<i32>) -> AccountInfo {
    let Some(value) = value.as_object() else {
        return AccountState::Unknown.into();
    };
    let logged_in = value.get("loggedIn").and_then(Value::as_bool);
    if code == Some(1) && logged_in == Some(false) {
        return AccountState::SignedOut.into();
    }
    if code != Some(0) || logged_in != Some(true) {
        return AccountState::Unknown.into();
    }
    AccountInfo {
        state: AccountState::SignedIn,
        email: display_text(value.get("email"), 320),
        account_id: None,
        username: None,
        plan: display_text(value.get("subscriptionType"), 80),
        authentication: display_text(value.get("authMethod"), 40),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_display_is_allowlisted_and_never_exports_tokens() {
        let raw = json!({
            "account": {"type": "chatgpt", "email": "dev@example.com", "planType": "pro", "accessToken": "SECRET"},
            "workspaceRouting": {"chatgptAccountId": "workspace-fixture", "bearer": "SECRET"},
            "refreshToken": "SECRET"
        });
        let value = codex_account(&raw);
        assert_eq!(value.state, AccountState::SignedIn);
        assert_eq!(value.email.as_deref(), Some("dev@example.com"));
        assert_eq!(value.account_id.as_deref(), Some("workspace-fixture"));
        assert!(!serde_json::to_string(&value).unwrap().contains("SECRET"));

        let value = claude_account(
            &json!({"loggedIn": true, "email": "dev@example.org", "subscriptionType": "max",
                    "authMethod": "claude.ai", "accessToken": "SECRET"}),
            Some(0),
        );
        assert_eq!(value.plan.as_deref(), Some("max"));
        assert!(!serde_json::to_string(&value).unwrap().contains("SECRET"));
        assert_eq!(
            claude_account(
                &json!({"loggedIn": false, "email": "stale@example.org"}),
                Some(1)
            ),
            AccountState::SignedOut.into()
        );
    }

    #[test]
    fn malformed_replies_are_unknown_not_signed_out() {
        assert_eq!(codex_account(&json!([])).state, AccountState::Unknown);
        assert_eq!(
            codex_account(&json!({"account": {"type": "other"}})).state,
            AccountState::Unknown
        );
        assert_eq!(
            codex_account(&json!({"account": null})).state,
            AccountState::SignedOut
        );
        assert_eq!(
            claude_account(&json!([]), Some(0)).state,
            AccountState::Unknown
        );
        assert_eq!(
            claude_account(&json!({"loggedIn": false}), Some(0)).state,
            AccountState::Unknown
        );
        assert_eq!(
            claude_account(&json!({"loggedIn": true}), Some(1)).state,
            AccountState::Unknown
        );
    }
}
