use homecloud_core::{GIB, Lifecycle, Resources, Vm, VmSpec};
use homecloud_host::{
    Backend, HostCommand, HostRequest, HostResponse, ObservingBackend, ProcessBackend,
};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn shutdown_requests_round_trip_through_python_dispatch_without_real_host_commands() {
    let backend = ObservingBackend {
        primary: Arc::new(ProcessBackend {
            command: HostCommand {
                program: "/usr/bin/python3".into(),
                args: vec![
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/shutdown_bridge.py")
                        .to_string_lossy()
                        .into_owned(),
                ],
            },
            sudo: false,
        }),
        observer: Arc::new(ProcessBackend {
            command: HostCommand {
                program: "/usr/bin/false".into(),
                args: vec![],
            },
            sudo: false,
        }),
    };
    let boot = "00000000-0000-4000-8000-000000000010".to_string();
    let operation_id = Uuid::parse_str("00000000-0000-4000-8000-000000000020").unwrap();
    let vm = Vm {
        id: Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
        owner: Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
        spec: VmSpec {
            name: "wire-test".into(),
            memory_gib: 2,
            cores: 1,
            disk_gib: 12,
            ssh_public_key: "fixture-only".into(),
            applications: vec![],
        },
        desired: Lifecycle::Running,
        observed: "running".into(),
        resources: Resources {
            memory_bytes: 2 * GIB,
            cpu_cores: 1,
            disk_bytes: 14 * GIB,
        },
        cpu_ids: vec![4, 10],
        network_slot: 1,
        ssh_host_key: None,
        gpus: vec![],
        preferred_gpus: vec![],
        pending_gpus: vec![],
        error: None,
    };
    assert!(
        matches!(backend.request(HostRequest::ShutdownStatus).await.unwrap(),HostResponse::ShutdownBoot(id) if id==boot)
    );
    for request in [
        HostRequest::DrainGuest {
            operation_id,
            boot_id: boot.clone(),
            vm: vm.clone(),
        },
        HostRequest::CheckShutdown {
            boot_id: boot.clone(),
            vms: vec![vm.clone()],
        },
        HostRequest::CheckShutdownFence {
            operation_id,
            boot_id: boot.clone(),
        },
        HostRequest::CancelShutdown {
            operation_id,
            boot_id: boot.clone(),
        },
    ] {
        assert!(matches!(
            backend.request(request).await.unwrap(),
            HostResponse::Ok
        ));
    }
    assert!(
        matches!(backend.request(HostRequest::PowerOff {operation_id,boot_id:boot.clone(),vms:vec![vm.clone()]}).await.unwrap(),HostResponse::PowerOffQueued {operation_id:id,boot_id} if id==operation_id && boot_id==boot)
    );
    let old_boot = Uuid::new_v4().to_string();
    assert!(
        matches!(backend.request(HostRequest::PowerOff {operation_id,boot_id:old_boot.clone(),vms:vec![vm]}).await.unwrap(),HostResponse::PowerOffRejected {operation_id:id,boot_id,message} if id==operation_id && boot_id==old_boot && message.contains("restarted"))
    );
}
