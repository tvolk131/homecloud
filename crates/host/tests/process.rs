use anyhow::{Result, bail};
use async_trait::async_trait;
use homecloud_host::{
    Backend, HostCommand, HostRequest, HostResponse, ObservingBackend, ProcessBackend,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct FailingBackend {
    name: &'static str,
    calls: AtomicUsize,
}

#[async_trait]
impl Backend for FailingBackend {
    async fn request(&self, _: HostRequest) -> Result<HostResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        bail!("{} failed", self.name)
    }
}

#[tokio::test]
async fn native_reads_do_not_fallback_and_mutations_still_go_directly_to_primary() {
    let backend = |name| {
        Arc::new(FailingBackend {
            name,
            calls: AtomicUsize::new(0),
        })
    };
    let primary = backend("primary");
    let observer = backend("observer");
    let routing = ObservingBackend {
        primary: primary.clone(),
        observer: observer.clone(),
    };
    for request in [HostRequest::Inventory, HostRequest::Observe { vms: vec![] }] {
        assert_eq!(
            routing.request(request).await.unwrap_err().to_string(),
            "observer failed"
        );
    }
    assert_eq!(primary.calls.load(Ordering::SeqCst), 0);
    for request in [
        HostRequest::ReserveStatus,
        HostRequest::Backing { users: vec![] },
    ] {
        assert_eq!(
            routing.request(request).await.unwrap_err().to_string(),
            "primary failed"
        );
    }
    assert_eq!(primary.calls.load(Ordering::SeqCst), 2);
    assert_eq!(observer.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn executable_and_arguments_preserve_json_error_protocol_without_shell_expansion() {
    let backend = ProcessBackend {
        command: HostCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "cat >/dev/null; printf '%s' \"$1\"".into(),
                "fixture".into(),
                r#"{"kind":"error","data":"literal spaces; $HOME"}"#.into(),
            ],
        },
        sudo: false,
    };
    assert_eq!(
        backend
            .request(HostRequest::Inventory)
            .await
            .unwrap_err()
            .to_string(),
        "literal spaces; $HOME"
    );
}

#[tokio::test]
async fn process_failures_and_invalid_json_are_not_successful_observations() {
    for (script, expected) in [
        (
            "cat >/dev/null; echo failed >&2; exit 1",
            "Host adapter: failed",
        ),
        (
            "cat >/dev/null; echo garbage",
            "Invalid host adapter response",
        ),
    ] {
        let backend = ProcessBackend {
            command: HostCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
            },
            sudo: false,
        };
        assert_eq!(
            backend
                .request(HostRequest::Inventory)
                .await
                .unwrap_err()
                .to_string()
                .trim(),
            expected
        );
    }
}

#[test]
fn observer_help_and_argument_rejection_do_not_require_root() {
    let binary = env!("CARGO_BIN_EXE_homecloud-host-observer");
    let help = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("read-only")
    );
    let unexpected = std::process::Command::new(binary)
        .args(["--root", "/tmp"])
        .output()
        .unwrap();
    assert!(!unexpected.status.success());
    assert!(
        String::from_utf8(unexpected.stderr)
            .unwrap()
            .contains("accepts no arguments")
    );
}

#[tokio::test]
async fn observer_requires_linux_and_privilege_before_reading_host_files() {
    if cfg!(target_os = "linux") && nix::unistd::geteuid().is_root() {
        return;
    }
    let backend = ProcessBackend {
        command: HostCommand {
            program: env!("CARGO_BIN_EXE_homecloud-host-observer").into(),
            args: vec![],
        },
        sudo: false,
    };
    assert!(
        backend
            .request(HostRequest::Inventory)
            .await
            .unwrap_err()
            .to_string()
            .contains(if cfg!(target_os = "linux") {
                "Run through the installed privileged adapter"
            } else {
                "The host observer requires Linux"
            })
    );
}
