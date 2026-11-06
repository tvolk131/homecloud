use crate::{
    PowerSubmission, PowerTaskState, ProxmoxConnection, VirtualizationBackend,
    parse_proxmox_resources,
};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use homecloud_core::{
    MAX_FRAME, VirtualizationGuestIdentity, VirtualizationInventory, VirtualizationPowerAction,
};
use http::{Method, Uri};
use proxmox_client::{Client, Error, HttpApiClient, TlsOptions, Token};
use std::{os::unix::fs::PermissionsExt, time::Duration};

pub struct ProxmoxVirtualization {
    client: Client,
    identity: String,
}

impl ProxmoxVirtualization {
    pub fn connect(config: ProxmoxConnection) -> Result<Self> {
        let endpoint: Uri = config
            .endpoint
            .parse()
            .context("Invalid Proxmox endpoint")?;
        ensure!(
            endpoint.scheme_str() == Some("https"),
            "Proxmox requires HTTPS"
        );
        ensure!(
            endpoint
                .authority()
                .is_some_and(|authority| !authority.as_str().contains('@'))
                && endpoint.path() == "/"
                && endpoint.query().is_none(),
            "Proxmox endpoint must contain only an HTTPS host and optional port"
        );
        ensure!(
            config.token_id.contains('@')
                && config.token_id.matches('!').count() == 1
                && !config.token_id.ends_with('!')
                && !config
                    .token_id
                    .chars()
                    .any(|character| character.is_whitespace()
                        || character.is_control()
                        || character == '='),
            "Invalid Proxmox token ID; expected user@realm!token"
        );
        let metadata = std::fs::symlink_metadata(&config.token_secret_file)
            .context("Cannot inspect Proxmox token secret file")?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
            "Proxmox token secret must be a private regular file"
        );
        ensure!(
            metadata.len() <= 4096,
            "Proxmox token secret file is too large"
        );
        let secret = std::fs::read_to_string(&config.token_secret_file)
            .context("Cannot read Proxmox token secret file")?;
        let secret = secret.trim_end_matches(['\r', '\n']);
        ensure!(
            !secret.is_empty()
                && !secret
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()),
            "Invalid Proxmox token secret"
        );
        let tls = match config.tls_fingerprint {
            Some(fingerprint) => TlsOptions::parse_fingerprint(&fingerprint)
                .context("Invalid Proxmox TLS fingerprint")?,
            None => TlsOptions::Verify,
        };
        let client = Client::with_options(endpoint, tls, Default::default())
            .map_err(|_| anyhow::anyhow!("Cannot initialize official Proxmox client"))?;
        let mut backend = Self::with_token(client, config.token_id, secret.into());
        backend.identity = format!("proxmox:{}", config.endpoint.trim_end_matches('/'));
        Ok(backend)
    }

    fn with_token(mut client: Client, token_id: String, secret: String) -> Self {
        client.set_pve_compatibility(true);
        client.set_authentication(Token {
            userid: token_id,
            prefix: "PVEAPIToken".into(),
            value: secret,
            perl_compat: true,
        });
        Self {
            client,
            identity: "proxmox:fixture".into(),
        }
    }

    fn validate_task(
        guest: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
        task: &str,
    ) -> Result<()> {
        guest.validate()?;
        ensure!(
            task.len() <= 512
                && task
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b":@.!_-".contains(&byte)),
            "Invalid Proxmox task identity"
        );
        let fields: Vec<_> = task.split(':').collect();
        let worker = match action {
            VirtualizationPowerAction::Start => "qmstart",
            VirtualizationPowerAction::Shutdown => "qmshutdown",
            VirtualizationPowerAction::ForceStop => "qmstop",
        };
        ensure!(
            fields.len() == 9
                && fields[0] == "UPID"
                && fields[1] == guest.node
                && fields[2..5]
                    .iter()
                    .all(|field| !field.is_empty()
                        && field.bytes().all(|byte| byte.is_ascii_hexdigit()))
                && fields[5] == worker
                && fields[6] == guest.vmid.to_string()
                && !fields[7].is_empty()
                && fields[8].is_empty(),
            "Proxmox returned a task for a different guest or action"
        );
        Ok(())
    }
}

#[async_trait]
impl VirtualizationBackend for ProxmoxVirtualization {
    fn identity(&self) -> Option<String> {
        Some(self.identity.clone())
    }
    async fn submit_power(
        &self,
        guest: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
    ) -> PowerSubmission {
        if let Err(error) = guest.validate() {
            return PowerSubmission::Rejected(error.to_string());
        }
        let command = match action {
            VirtualizationPowerAction::Start => "start",
            VirtualizationPowerAction::Shutdown => "shutdown",
            VirtualizationPowerAction::ForceStop => "stop",
        };
        let path = format!(
            "/api2/extjs/nodes/{}/qemu/{}/status/{command}",
            guest.node, guest.vmid
        );
        let parameters = if action == VirtualizationPowerAction::Shutdown {
            serde_json::json!({"timeout": 60, "forceStop": 0})
        } else {
            serde_json::json!({})
        };
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.client.request(Method::POST, &path, Some(parameters)),
        )
        .await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(Error::Api(status, _))) if status.is_client_error() && status.as_u16() != 408 => {
                return PowerSubmission::Rejected(format!("Proxmox rejected the power request (HTTP {}); check VM permissions and configuration", status.as_u16()));
            }
            Ok(Err(Error::Unauthorized)) => return PowerSubmission::Rejected("Proxmox power authentication failed".into()),
            _ => return PowerSubmission::Unknown("Power submission was interrupted or timed out. The action may have reached Proxmox; it will not be retried automatically.".into()),
        };
        if response.body.len() > MAX_FRAME {
            return PowerSubmission::Unknown(
                "Proxmox power response exceeds the response limit".into(),
            );
        }
        if let Some(status) = crate::explicit_power_rejection(&response.body) {
            return PowerSubmission::Rejected(format!(
                "Proxmox rejected the power request (HTTP {status}); check VM permissions and configuration"
            ));
        }
        let data = match response.expect_json::<String>() {
            Ok(data) => data,
            Err(_) => return PowerSubmission::Unknown("Proxmox did not return a valid power task. Inspect Proxmox before releasing this safety hold.".into()),
        };
        match Self::validate_task(guest, action, &data.data) {
            Ok(()) => PowerSubmission::Accepted(data.data),
            Err(error) => PowerSubmission::Unknown(error.to_string()),
        }
    }

    async fn power_task(
        &self,
        guest: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
        task: &str,
    ) -> Result<PowerTaskState> {
        Self::validate_task(guest, action, task)?;
        let path = format!("/api2/extjs/nodes/{}/tasks/{task}/status", guest.node);
        let response = tokio::time::timeout(Duration::from_secs(15), self.client.request(Method::GET, &path, None::<()>))
            .await.context("Proxmox task status timed out; tracking will resume")?
            .map_err(|_| anyhow::anyhow!("Cannot read Proxmox task status; check connectivity, TLS and task audit permissions"))?;
        ensure!(
            response.body.len() <= MAX_FRAME,
            "Proxmox task response exceeds the response limit"
        );
        let response = response
            .expect_json::<serde_json::Value>()
            .map_err(|_| anyhow::anyhow!("Invalid Proxmox task status response"))?;
        match response.data.get("status").and_then(|value| value.as_str()) {
            Some("running") => Ok(PowerTaskState::Running),
            Some("stopped") => match response
                .data
                .get("exitstatus")
                .and_then(|value| value.as_str())
            {
                Some("OK") => Ok(PowerTaskState::Succeeded),
                Some(_) => Ok(PowerTaskState::Failed),
                None => {
                    anyhow::bail!("Stopped Proxmox task has no exit status; tracking will resume")
                }
            },
            _ => anyhow::bail!("Unknown Proxmox task status; tracking will resume"),
        }
    }

    async fn inventory(&self) -> Result<VirtualizationInventory> {
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.client
                .request(Method::GET, "/api2/extjs/cluster/resources", None::<()>),
        )
        .await
        .context("Proxmox inventory request timed out")?
        .map_err(|error| match error {
            Error::Api(status, _) => anyhow::anyhow!(
                "Proxmox inventory request failed (HTTP {})",
                status.as_u16()
            ),
            Error::Unauthorized => anyhow::anyhow!("Proxmox inventory authentication failed"),
            _ => anyhow::anyhow!(
                "Proxmox inventory connection failed; check endpoint, TLS trust and service token"
            ),
        })?;
        ensure!(
            response.body.len() <= MAX_FRAME,
            "Proxmox inventory exceeds the response limit"
        );
        let data = response
            .expect_json::<serde_json::Value>()
            .map_err(|_| anyhow::anyhow!("Invalid Proxmox inventory response"))?;
        parse_proxmox_resources(data.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homecloud_core::VirtualizationSource;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock_server(status: &str, body: &str) -> (Uri, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() < 8192);
                request.extend_from_slice(&buffer[..count]);
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < header_end + length {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() < 8192);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (endpoint, task)
    }

    #[tokio::test]
    async fn official_client_uses_pve_token_header_and_extjs_envelope() {
        let (endpoint, request) = mock_server(
            "200 OK",
            r#"{"success":1,"data":[{"type":"qemu","node":"pve","vmid":101,"status":"running"}]}"#,
        )
        .await;
        let backend = ProxmoxVirtualization::with_token(
            Client::new(endpoint),
            "reader@pve!homecloud".into(),
            "synthetic-secret".into(),
        );
        let inventory = backend.inventory().await.unwrap();
        assert_eq!(inventory.source, VirtualizationSource::Proxmox);
        assert_eq!(inventory.guests[0].vmid, 101);
        let request = request.await.unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /api2/extjs/cluster/resources http/1.1\r\n"));
        assert!(
            request
                .contains("authorization: pveapitoken=reader@pve!homecloud=synthetic-secret\r\n")
        );
    }

    #[tokio::test]
    async fn upstream_errors_do_not_echo_response_bodies_or_credentials() {
        let (endpoint, request) =
            mock_server("403 Forbidden", r#"{"message":"synthetic-secret"}"#).await;
        let backend = ProxmoxVirtualization::with_token(
            Client::new(endpoint),
            "reader@pve!homecloud".into(),
            "synthetic-secret".into(),
        );
        let error = backend.inventory().await.unwrap_err().to_string();
        assert!(error.contains("403"));
        assert!(!error.contains("synthetic-secret"));
        request.await.unwrap();
    }

    #[tokio::test]
    async fn unsuccessful_envelopes_are_not_empty_successful_inventories() {
        let (endpoint, request) =
            mock_server("200 OK", r#"{"success":0,"message":"denied","data":[]}"#).await;
        let backend = ProxmoxVirtualization::with_token(
            Client::new(endpoint),
            "reader@pve!homecloud".into(),
            "synthetic-secret".into(),
        );
        assert!(backend.inventory().await.is_err());
        request.await.unwrap();
    }

    fn power_guest() -> VirtualizationGuestIdentity {
        VirtualizationGuestIdentity {
            node: "pve".into(),
            vmid: 101,
            kind: homecloud_core::VirtualizationGuestKind::VirtualMachine,
        }
    }

    #[tokio::test]
    async fn shutdown_returns_native_task_and_never_enables_force_fallback() {
        let task = "UPID:pve:00000001:00000002:00000003:qmshutdown:101:owner@pve!test:";
        let body = serde_json::json!({"success":1,"data":task}).to_string();
        let (endpoint, request) = mock_server("200 OK", &body).await;
        let backend = ProxmoxVirtualization::with_token(
            Client::new(endpoint),
            "owner@pve!test".into(),
            "synthetic-secret".into(),
        );
        assert!(
            matches!(backend.submit_power(&power_guest(),VirtualizationPowerAction::Shutdown).await, PowerSubmission::Accepted(actual) if actual == task)
        );
        let request = request.await.unwrap();
        assert!(
            request.starts_with("POST /api2/extjs/nodes/pve/qemu/101/status/shutdown HTTP/1.1\r\n")
        );
        let parameters: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(parameters, serde_json::json!({"timeout":60,"forceStop":0}));
    }

    #[tokio::test]
    async fn start_and_force_stop_are_separate_task_types_and_endpoints() {
        for (action, command, worker) in [
            (VirtualizationPowerAction::Start, "start", "qmstart"),
            (VirtualizationPowerAction::ForceStop, "stop", "qmstop"),
        ] {
            let task = format!("UPID:pve:00000001:00000002:00000003:{worker}:101:owner@pve!test:");
            let (endpoint, request) = mock_server(
                "200 OK",
                &serde_json::json!({"success":1,"data":task}).to_string(),
            )
            .await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            assert!(
                matches!(backend.submit_power(&power_guest(),action).await, PowerSubmission::Accepted(actual) if actual == task)
            );
            assert!(request.await.unwrap().starts_with(&format!(
                "POST /api2/extjs/nodes/pve/qemu/101/status/{command} HTTP/1.1\r\n"
            )));
        }
    }

    #[tokio::test]
    async fn power_failures_distinguish_rejection_from_ambiguous_dispatch_without_leaks() {
        for (status, rejected) in [
            ("403 Forbidden", true),
            ("401 Unauthorized", true),
            ("500 Internal Server Error", false),
            ("408 Request Timeout", false),
        ] {
            let (endpoint, request) =
                mock_server(status, r#"{"message":"synthetic-secret"}"#).await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            let error = match backend
                .submit_power(&power_guest(), VirtualizationPowerAction::Start)
                .await
            {
                PowerSubmission::Rejected(error) => {
                    assert!(rejected);
                    error
                }
                PowerSubmission::Unknown(error) => {
                    assert!(!rejected);
                    error
                }
                PowerSubmission::Accepted(_) => {
                    panic!("An unsuccessful response cannot accept a power task")
                }
            };
            assert!(!error.contains("synthetic-secret"));
            request.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_or_cross_guest_tasks_remain_uncertain_not_successful() {
        for task in [
            "invalid",
            "UPID:pve:1:2:3:qmstart:102:owner@pve:",
            "UPID:other:1:2:3:qmstart:101:owner@pve:",
            "UPID:pve:1:2:3:qmstop:101:owner@pve:",
            "UPID:pve:1:2:3:qmstart:101:owner@pve:/status",
        ] {
            let (endpoint, request) = mock_server(
                "200 OK",
                &serde_json::json!({"success":1,"data":task}).to_string(),
            )
            .await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            assert!(matches!(
                backend
                    .submit_power(&power_guest(), VirtualizationPowerAction::Start)
                    .await,
                PowerSubmission::Unknown(_)
            ));
            request.await.unwrap();
        }
    }

    #[tokio::test]
    async fn task_status_requires_confirmed_terminal_exit_status() {
        let task = "UPID:pve:1:2:3:qmshutdown:101:owner@pve!test:";
        for (status, expected) in [
            (
                serde_json::json!({"status":"running"}),
                Some(PowerTaskState::Running),
            ),
            (
                serde_json::json!({"status":"stopped","exitstatus":"OK"}),
                Some(PowerTaskState::Succeeded),
            ),
            (
                serde_json::json!({"status":"stopped","exitstatus":"guest shutdown failed"}),
                Some(PowerTaskState::Failed),
            ),
            (serde_json::json!({"status":"stopped"}), None),
            (serde_json::json!({"status":"unknown"}), None),
        ] {
            let (endpoint, request) = mock_server(
                "200 OK",
                &serde_json::json!({"success":1,"data":status}).to_string(),
            )
            .await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            let actual = backend
                .power_task(&power_guest(), VirtualizationPowerAction::Shutdown, task)
                .await;
            match expected {
                Some(expected) => assert_eq!(actual.unwrap(), expected),
                None => assert!(actual.is_err()),
            }
            assert!(request.await.unwrap().starts_with(&format!(
                "GET /api2/extjs/nodes/pve/tasks/{task}/status HTTP/1.1\r\n"
            )));
        }
    }

    #[tokio::test]
    async fn invalid_targets_are_rejected_before_connecting() {
        let backend = ProxmoxVirtualization::with_token(
            Client::new("http://127.0.0.1:1/".parse().unwrap()),
            "owner@pve!test".into(),
            "synthetic-secret".into(),
        );
        for guest in [
            VirtualizationGuestIdentity {
                node: "../node".into(),
                ..power_guest()
            },
            VirtualizationGuestIdentity {
                vmid: 0,
                ..power_guest()
            },
            VirtualizationGuestIdentity {
                kind: homecloud_core::VirtualizationGuestKind::Container,
                ..power_guest()
            },
        ] {
            assert!(matches!(
                backend
                    .submit_power(&guest, VirtualizationPowerAction::Start)
                    .await,
                PowerSubmission::Rejected(_)
            ));
        }
    }

    #[tokio::test]
    async fn incomplete_http_success_envelopes_keep_the_power_request_uncertain() {
        for body in [r#"{"data":"UPID:accepted-task"}"#, r#"{"success":0}"#] {
            let (endpoint, request) = mock_server("200 OK", body).await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            assert!(matches!(
                backend
                    .submit_power(&power_guest(), VirtualizationPowerAction::Start)
                    .await,
                PowerSubmission::Unknown(_)
            ));
            request.await.unwrap();
        }
    }

    #[tokio::test]
    async fn extjs_rejections_inside_http_success_are_not_ambiguous_or_secret_leaks() {
        for (status, rejected) in [(403, true), (500, false), (408, false)] {
            let body =
                serde_json::json!({"success":0,"status":status,"message":"synthetic-secret"})
                    .to_string();
            let (endpoint, request) = mock_server("200 OK", &body).await;
            let backend = ProxmoxVirtualization::with_token(
                Client::new(endpoint),
                "owner@pve!test".into(),
                "synthetic-secret".into(),
            );
            let error = match backend
                .submit_power(&power_guest(), VirtualizationPowerAction::Start)
                .await
            {
                PowerSubmission::Rejected(error) => {
                    assert!(rejected);
                    error
                }
                PowerSubmission::Unknown(error) => {
                    assert!(!rejected);
                    error
                }
                PowerSubmission::Accepted(_) => panic!("An extjs rejection cannot accept a task"),
            };
            assert!(!error.contains("synthetic-secret"));
            request.await.unwrap();
        }
    }

    #[test]
    fn production_connection_rejects_http_and_public_secret_files() {
        let directory = tempfile::tempdir().unwrap();
        let secret = directory.path().join("token");
        std::fs::write(&secret, "synthetic-secret").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        let config = |endpoint: &str| ProxmoxConnection {
            endpoint: endpoint.into(),
            token_id: "reader@pve!homecloud".into(),
            token_secret_file: secret.clone(),
            tls_fingerprint: None,
        };
        assert!(ProxmoxVirtualization::connect(config("http://127.0.0.1:8006/")).is_err());
        assert!(ProxmoxVirtualization::connect(config("https://pve:8006/")).is_err());
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ProxmoxVirtualization::connect(config("https://pve:8006/")).is_ok());
        assert!(ProxmoxVirtualization::connect(config("https://user:password@pve:8006/")).is_err());
        assert!(ProxmoxVirtualization::connect(config("https://pve:8006/api2/json")).is_err());
    }
}
