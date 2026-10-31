use super::*;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use uuid::Uuid;

struct PowerBackend {
    simulator: homecloud_host::Simulator,
    boot: StdMutex<String>,
    fail_drain: AtomicBool,
    fail_check: AtomicBool,
    fail_power: AtomicBool,
    reject_power: AtomicBool,
    mismatched_rejection: AtomicBool,
    policy: StdMutex<Option<HostReservePolicy>>,
    drains: AtomicUsize,
    live_vms: StdMutex<std::collections::HashMap<Uuid, String>>,
    delay_power_reply: AtomicBool,
    queued_reply_at: AtomicU64,
    powers: AtomicUsize,
    pause_apply: AtomicBool,
    cancelled: StdMutex<std::collections::HashSet<Uuid>>,
    lose_cancel_reply: AtomicBool,
    apply_entered: tokio::sync::Notify,
    apply_release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Backend for PowerBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        match request {
            HostRequest::ShutdownStatus => Ok(HostResponse::ShutdownBoot(
                self.boot.lock().unwrap().clone(),
            )),
            HostRequest::CancelShutdown { operation_id, .. } => {
                self.cancelled.lock().unwrap().insert(operation_id);
                ensure!(
                    !self.lose_cancel_reply.swap(false, Ordering::SeqCst),
                    "Lost cancellation reply"
                );
                Ok(HostResponse::Ok)
            }
            HostRequest::CheckShutdownFence { operation_id, .. } => {
                ensure!(
                    !self.cancelled.lock().unwrap().contains(&operation_id),
                    "Cancellation was fenced; finish cancellation"
                );
                Ok(HostResponse::Ok)
            }
            HostRequest::DrainGuest {
                operation_id, vm, ..
            } => {
                ensure!(
                    !self.cancelled.lock().unwrap().contains(&operation_id),
                    "Shutdown was cancelled"
                );
                ensure!(
                    self.live_vms.lock().unwrap().contains_key(&vm.id),
                    "Recorded VM instance is missing"
                );
                self.drains.fetch_add(1, Ordering::SeqCst);
                ensure!(
                    !self.fail_drain.load(Ordering::SeqCst),
                    "Guest did not stop"
                );
                self.live_vms
                    .lock()
                    .unwrap()
                    .insert(vm.id, "stopped".into());
                Ok(HostResponse::Ok)
            }
            HostRequest::CheckShutdown { .. } => {
                ensure!(
                    !self.fail_check.load(Ordering::SeqCst),
                    "An unmanaged guest is running"
                );
                Ok(HostResponse::Ok)
            }
            HostRequest::PowerOff {
                operation_id,
                boot_id,
                ..
            } => {
                if self.reject_power.load(Ordering::SeqCst) {
                    return Ok(HostResponse::PowerOffRejected {
                        operation_id: if self.mismatched_rejection.load(Ordering::SeqCst) {
                            Uuid::new_v4()
                        } else {
                            operation_id
                        },
                        boot_id,
                        message: "A guest started before the power-off command".into(),
                    });
                }
                self.powers.fetch_add(1, Ordering::SeqCst);
                ensure!(
                    !self.fail_power.load(Ordering::SeqCst),
                    "Lost power-off reply"
                );
                if self.delay_power_reply.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                }
                self.queued_reply_at
                    .store(homecloud_store::now(), Ordering::SeqCst);
                Ok(HostResponse::PowerOffQueued {
                    operation_id,
                    boot_id,
                })
            }
            HostRequest::ReserveStatus => {
                let mut policy = self.policy.lock().unwrap().clone();
                if let Some(p) = &mut policy
                    && let Some(m) = &mut p.memory
                {
                    m.boot_id = self.boot.lock().unwrap().clone();
                    m.observed_at = homecloud_store::now();
                }
                Ok(HostResponse::ReservePolicy(policy))
            }
            HostRequest::Observe { vms } => {
                let live = self.live_vms.lock().unwrap();
                Ok(HostResponse::Observations(
                    vms.into_iter()
                        .filter(|vm| vm.observed != "deleted")
                        .map(|vm| {
                            let state = live
                                .get(&vm.id)
                                .cloned()
                                .unwrap_or_else(|| "missing".into());
                            homecloud_host::VmObservation {
                                id: vm.id,
                                error: (state == "missing")
                                    .then(|| "Recorded VM is missing from Incus".into()),
                                state,
                                progress: None,
                            }
                        })
                        .collect(),
                ))
            }
            request => {
                if matches!(&request, HostRequest::Apply { .. })
                    && self.pause_apply.swap(false, Ordering::SeqCst)
                {
                    self.apply_entered.notify_one();
                    self.apply_release.notified().await;
                }
                let applied = match &request {
                    HostRequest::Apply { vm, .. } => Some(vm.id),
                    _ => None,
                };
                let response = self.simulator.request(request).await?;
                if let (Some(id), HostResponse::Applied(observation)) = (applied, &response) {
                    self.live_vms
                        .lock()
                        .unwrap()
                        .insert(id, observation.state.clone());
                }
                Ok(response)
            }
        }
    }
}
async fn fixture() -> (tempfile::TempDir, Arc<Service>, Arc<PowerBackend>, Uuid) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
    let (user, token) = store.bootstrap("Admin").unwrap();
    store.enroll(&token, "admin").unwrap();
    store
        .grant(
            "admin",
            user.id,
            Resources {
                memory_bytes: 4 * GIB,
                cpu_cores: 1,
                disk_bytes: 32 * GIB,
            },
            false,
        )
        .unwrap();
    let backend = Arc::new(PowerBackend {
        simulator: homecloud_host::Simulator::new(dir.path().join("observed.json")),
        boot: StdMutex::new(Uuid::new_v4().to_string()),
        fail_drain: AtomicBool::new(false),
        fail_check: AtomicBool::new(false),
        fail_power: AtomicBool::new(false),
        reject_power: AtomicBool::new(false),
        mismatched_rejection: AtomicBool::new(false),
        policy: StdMutex::new(None),
        drains: AtomicUsize::new(0),
        live_vms: StdMutex::new(std::collections::HashMap::new()),
        delay_power_reply: AtomicBool::new(false),
        queued_reply_at: AtomicU64::new(0),
        powers: AtomicUsize::new(0),
        pause_apply: AtomicBool::new(false),
        cancelled: StdMutex::new(std::collections::HashSet::new()),
        lose_cancel_reply: AtomicBool::new(false),
        apply_entered: tokio::sync::Notify::new(),
        apply_release: tokio::sync::Notify::new(),
    });
    let service = Service::new(store, backend.clone()).unwrap();
    let op = service
        .store
        .submit(
            "admin",
            Uuid::new_v4(),
            Mutation::Create {
                spec: VmSpec {
                    name: "devbox".into(),
                    memory_gib: 2,
                    cores: 1,
                    disk_gib: 12,
                    ssh_public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixturePublicKey".into(),
                    applications: vec![],
                },
            },
        )
        .unwrap();
    service.reconcile_once().await.unwrap();
    assert_eq!(service.store.vm(op.vm_id).unwrap().observed, "running");
    (dir, service, backend, op.vm_id)
}
async fn request(service: &Service, key: Uuid) -> HostShutdownOperation {
    let boot = service.shutdown_boot().await.unwrap();
    match service
        .request(
            "admin".into(),
            Request::ShutdownHost {
                idempotency_key: key,
                expected_boot_id: boot,
            },
        )
        .await
        .unwrap()
    {
        Response::HostShutdownOperation(op) => op,
        _ => panic!("Missing shutdown receipt"),
    }
}
#[tokio::test]
async fn drain_keeps_intent_and_claims_and_poweroff_is_only_attempted_once() {
    let (_dir, s, b, id) = fixture().await;
    let before = s.store.vm(id).unwrap();
    let users = s.store.users().unwrap();
    let key = Uuid::new_v4();
    let op = request(&s, key).await;
    assert_eq!(request(&s, key).await.id, op.id);
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
    s.reconcile_once().await.unwrap();
    let after = s.store.vm(id).unwrap();
    assert_eq!(after.desired, Lifecycle::Running);
    assert_eq!(after.observed, "stopped");
    assert_eq!(after.resources, before.resources);
    assert_eq!(after.cpu_ids, before.cpu_ids);
    assert_eq!(after.gpus, before.gpus);
    assert_eq!(
        serde_json::to_value(s.store.users().unwrap()).unwrap(),
        serde_json::to_value(users).unwrap()
    );
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(b.drains.load(Ordering::SeqCst), 1);
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.store.host_shutdown().unwrap().unwrap().phase,
        HostShutdownPhase::PoweroffRequested
    );
}
#[tokio::test]
async fn shutdown_is_admin_only_fences_work_and_preserves_snapshot_access() {
    let (_dir, s, _b, id) = fixture().await;
    let (token, _, _) = s
        .store
        .invite(
            "admin",
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.store.enroll(&token, "member").unwrap();
    for req in [
        Request::ShutdownHost {
            idempotency_key: Uuid::new_v4(),
            expected_boot_id: s.shutdown_boot().await.unwrap(),
        },
        Request::RetryHostShutdown {
            operation_id: Uuid::new_v4(),
        },
        Request::CancelHostShutdown {
            operation_id: Uuid::new_v4(),
        },
    ] {
        assert!(
            s.request("member".into(), req)
                .await
                .unwrap_err()
                .to_string()
                .contains("Administrator")
        );
    }
    request(&s, Uuid::new_v4()).await;
    for req in [
        Request::Mutate {
            idempotency_key: Uuid::new_v4(),
            mutation: Mutation::State {
                vm_id: id,
                desired: Lifecycle::Stopped,
            },
        },
        Request::RetryOperation {
            operation_id: Uuid::new_v4(),
        },
        Request::EnsureApplication {
            vm_id: id,
            application_kind: ApplicationKind::T3,
            idempotency_key: Uuid::new_v4(),
        },
    ] {
        assert!(
            s.request("admin".into(), req)
                .await
                .unwrap_err()
                .to_string()
                .contains("shutdown")
        );
    }
    match s.request("member".into(), Request::Snapshot).await.unwrap() {
        Response::Snapshot(snapshot) => {
            let op = snapshot.host_shutdown.unwrap();
            assert!(op.vm_ids.is_empty());
            assert!(op.remaining.is_empty());
            assert!(op.error.is_none());
        }
        _ => panic!("Missing snapshot"),
    }
    s.reconcile_applications_once().await.unwrap();
}
#[tokio::test]
async fn failed_drain_never_powers_off_and_cancel_explicitly_resumes_guests() {
    let (_dir, s, b, id) = fixture().await;
    let op = request(&s, Uuid::new_v4()).await;
    b.fail_drain.store(true, Ordering::SeqCst);
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(b.drains.load(Ordering::SeqCst), 1);
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
    let failed = s.store.host_shutdown().unwrap().unwrap();
    assert_eq!(failed.phase, HostShutdownPhase::NeedsReview);
    assert!(failed.error.unwrap().contains("devbox"));
    s.request(
        "admin".into(),
        Request::CancelHostShutdown {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    // Replay a lost cancellation reply without scheduling more starts.
    s.request(
        "admin".into(),
        Request::CancelHostShutdown {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    assert_eq!(s.store.pending().unwrap().len(), 1);
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.vm(id).unwrap().observed, "running");
    assert!(
        !s.store
            .host_shutdown()
            .unwrap()
            .unwrap()
            .phase
            .blocks_work()
    );
}
#[tokio::test]
async fn validation_failure_can_be_cancelled_but_ambiguous_poweroff_cannot() {
    let (_dir, s, b, _id) = fixture().await;
    let op = request(&s, Uuid::new_v4()).await;
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    b.fail_check.store(true, Ordering::SeqCst);
    s.reconcile_once().await.unwrap();
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
    assert!(!s.store.host_shutdown().unwrap().unwrap().poweroff_attempted);
    b.fail_check.store(false, Ordering::SeqCst);
    b.fail_power.store(true, Ordering::SeqCst);
    s.request(
        "admin".into(),
        Request::RetryHostShutdown {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
    assert!(
        s.request(
            "admin".into(),
            Request::CancelHostShutdown {
                operation_id: op.id
            }
        )
        .await
        .is_err()
    );
    s.reconcile_once().await.unwrap();
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn daemon_restart_requires_review_and_new_kernel_boot_clears_the_fence() {
    let (_dir, s, b, _id) = fixture().await;
    let key = Uuid::new_v4();
    let op = request(&s, key).await;
    let store = s.store.clone();
    drop(s);
    let s = Service::new(store, b.clone()).unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(
        s.store.host_shutdown().unwrap().unwrap().phase,
        HostShutdownPhase::NeedsReview
    );
    assert_eq!(b.drains.load(Ordering::SeqCst), 0);
    *b.boot.lock().unwrap() = Uuid::new_v4().to_string();
    s.reconcile_once().await.unwrap();
    assert_eq!(
        s.store.host_shutdown().unwrap().unwrap().phase,
        HostShutdownPhase::Finished
    );
    let old = s
        .store
        .request_host_shutdown("admin", key, op.boot_id)
        .unwrap();
    assert_eq!(old.phase, HostShutdownPhase::Finished);
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn pending_vm_operations_and_stale_boots_reject_shutdown_before_effects() {
    let (_dir, s, b, id) = fixture().await;
    assert!(
        s.request(
            "admin".into(),
            Request::ShutdownHost {
                idempotency_key: Uuid::new_v4(),
                expected_boot_id: Uuid::new_v4().to_string()
            }
        )
        .await
        .is_err()
    );
    s.store
        .submit(
            "admin",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    assert!(
        s.request(
            "admin".into(),
            Request::ShutdownHost {
                idempotency_key: Uuid::new_v4(),
                expected_boot_id: s.shutdown_boot().await.unwrap()
            }
        )
        .await
        .is_err()
    );
    assert!(s.store.host_shutdown().unwrap().is_none());
    assert_eq!(b.drains.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pending_host_work_rejects_shutdown_without_delaying_status_requests() {
    let (_dir, s, b, id) = fixture().await;
    s.store
        .submit(
            "admin",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    b.pause_apply.store(true, Ordering::SeqCst);
    let worker = {
        let s = s.clone();
        tokio::spawn(async move { s.reconcile_once().await })
    };
    b.apply_entered.notified().await;
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        s.request(
            "admin".into(),
            Request::ShutdownHost {
                idempotency_key: Uuid::new_v4(),
                expected_boot_id: s.shutdown_boot().await.unwrap(),
            },
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().to_string().contains("pending VM"));
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            s.request("admin".into(), Request::Snapshot)
        )
        .await
        .unwrap()
        .unwrap(),
        Response::Snapshot(_)
    ));
    assert!(s.store.host_shutdown().unwrap().is_none());
    b.apply_release.notify_one();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn waiting_shutdown_times_out_without_blocking_snapshots_or_device_revocation() {
    let (_dir, s, _b, _id) = fixture().await;
    let user = s.store.authenticate("admin").unwrap();
    let (token, _) = s.store.invite_device("admin", user.id, 1).unwrap();
    s.store.enroll(&token, "lost-device").unwrap();
    let guard = s.shutdown_gate.read().await;
    let shutdown = {
        let s = s.clone();
        tokio::spawn(async move {
            s.request(
                "admin".into(),
                Request::ShutdownHost {
                    idempotency_key: Uuid::new_v4(),
                    expected_boot_id: s.shutdown_boot().await.unwrap(),
                },
            )
            .await
        })
    };
    // Wait until the writer is queued. A read would otherwise still succeed.
    tokio::time::timeout(Duration::from_secs(1), async {
        while s.shutdown_gate.try_read().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            s.request("admin".into(), Request::Snapshot)
        )
        .await
        .unwrap()
        .unwrap(),
        Response::Snapshot(_)
    ));
    tokio::time::timeout(
        Duration::from_millis(500),
        s.request(
            "admin".into(),
            Request::RevokeDevice {
                peer: "lost-device".into(),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(s.store.authenticate("lost-device").is_err());
    assert!(
        shutdown
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("host is busy")
    );
    assert!(s.store.host_shutdown().unwrap().is_none());
    drop(guard);
}

#[tokio::test]
async fn restarting_after_a_poweroff_attempt_cannot_automatically_repeat_it() {
    let (_dir, s, b, _id) = fixture().await;
    request(&s, Uuid::new_v4()).await;
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    let store = s.store.clone();
    drop(s);
    let s = Service::new(store, b.clone()).unwrap();
    s.reconcile_once().await.unwrap();
    let op = s.store.host_shutdown().unwrap().unwrap();
    assert_eq!(op.phase, HostShutdownPhase::NeedsReview);
    assert!(op.poweroff_attempted);
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
    assert!(
        s.request(
            "admin".into(),
            Request::CancelHostShutdown {
                operation_id: op.id
            }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn lost_cancellation_reply_fences_retry_and_leaves_work_paused_until_confirmed() {
    let (_dir, s, b, _id) = fixture().await;
    let op = request(&s, Uuid::new_v4()).await;
    b.fail_drain.store(true, Ordering::SeqCst);
    s.reconcile_once().await.unwrap();
    b.lose_cancel_reply.store(true, Ordering::SeqCst);
    assert!(
        s.request(
            "admin".into(),
            Request::CancelHostShutdown {
                operation_id: op.id
            }
        )
        .await
        .is_err()
    );
    assert!(
        s.store
            .host_shutdown()
            .unwrap()
            .unwrap()
            .phase
            .blocks_work()
    );
    assert!(s.store.pending().unwrap().is_empty());
    assert!(
        s.request(
            "admin".into(),
            Request::RetryHostShutdown {
                operation_id: op.id
            }
        )
        .await
        .is_err()
    );
    s.request(
        "admin".into(),
        Request::CancelHostShutdown {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    assert_eq!(s.store.pending().unwrap().len(), 1);
}

#[tokio::test]
async fn security_actions_stay_available_while_poweroff_is_awaiting_recovery() {
    let (_dir, s, b, _id) = fixture().await;
    let user = s.store.authenticate("admin").unwrap();
    let (token, _) = s.store.invite_device("admin", user.id, 1).unwrap();
    s.store.enroll(&token, "lost-device").unwrap();
    let (token, member, _) = s
        .store
        .invite(
            "admin",
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.store.enroll(&token, "member").unwrap();
    request(&s, Uuid::new_v4()).await;
    b.fail_power.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    s.request(
        "admin".into(),
        Request::RevokeDevice {
            peer: "lost-device".into(),
        },
    )
    .await
    .unwrap();
    s.request(
        "admin".into(),
        Request::SetAccountActive {
            user_id: member,
            active: false,
        },
    )
    .await
    .unwrap();
    assert!(s.store.authenticate("lost-device").is_err());
    assert!(s.store.authenticate("member").is_err());
    assert!(matches!(
        s.request(
            "admin".into(),
            Request::InviteDevice {
                user_id: user.id,
                expires_hours: 1
            }
        )
        .await
        .unwrap(),
        Response::Invitation { .. }
    ));
    assert!(
        s.store
            .host_shutdown()
            .unwrap()
            .unwrap()
            .phase
            .blocks_work()
    );
}

#[tokio::test]
async fn acknowledged_poweroff_refusal_allows_cancel_but_never_erases_an_uncertain_attempt() {
    for uncertain in [false, true] {
        let (_dir, s, b, _id) = fixture().await;
        let op = request(&s, Uuid::new_v4()).await;
        if uncertain {
            b.fail_power.store(true, Ordering::SeqCst);
            for _ in 0..3 {
                s.reconcile_once().await.unwrap();
            }
            s.request(
                "admin".into(),
                Request::RetryHostShutdown {
                    operation_id: op.id,
                },
            )
            .await
            .unwrap();
        }
        b.reject_power.store(true, Ordering::SeqCst);
        for _ in 0..3 {
            s.reconcile_once().await.unwrap();
        }
        let refused = s.store.host_shutdown().unwrap().unwrap();
        assert_eq!(refused.phase, HostShutdownPhase::NeedsReview);
        assert_eq!(refused.poweroff_attempted, uncertain);
        let cancel = s
            .request(
                "admin".into(),
                Request::CancelHostShutdown {
                    operation_id: op.id,
                },
            )
            .await;
        assert_eq!(cancel.is_err(), uncertain);
    }
}

#[tokio::test]
async fn mismatched_poweroff_rejection_cannot_restore_cancellation() {
    let (_dir, s, b, _id) = fixture().await;
    let op = request(&s, Uuid::new_v4()).await;
    b.reject_power.store(true, Ordering::SeqCst);
    b.mismatched_rejection.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    assert!(s.store.host_shutdown().unwrap().unwrap().poweroff_attempted);
    assert!(
        s.request(
            "admin".into(),
            Request::CancelHostShutdown {
                operation_id: op.id
            }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn acknowledged_poweroff_that_leaves_the_host_reachable_times_out_to_review() {
    let (_dir, s, b, _id) = fixture().await;
    request(&s, Uuid::new_v4()).await;
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    let mut op = s.store.host_shutdown().unwrap().unwrap();
    op.poweroff_requested_at = Some(homecloud_store::now() - 61);
    s.store.update_host_shutdown(op).unwrap();
    s.reconcile_once().await.unwrap();
    let op = s.store.host_shutdown().unwrap().unwrap();
    assert_eq!(op.phase, HostShutdownPhase::NeedsReview);
    assert!(op.poweroff_attempted);
    assert!(op.error.unwrap().contains("still reachable"));
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_replay_preserves_progress_without_draining_again() {
    let (_dir, s, b, _id) = fixture().await;
    let op = request(&s, Uuid::new_v4()).await;
    b.fail_drain.store(true, Ordering::SeqCst);
    s.reconcile_once().await.unwrap();
    b.fail_drain.store(false, Ordering::SeqCst);
    let retry = Request::RetryHostShutdown {
        operation_id: op.id,
    };
    s.request("admin".into(), retry.clone()).await.unwrap();
    s.reconcile_once().await.unwrap();
    let before = s.store.host_shutdown().unwrap().unwrap();
    s.request("admin".into(), retry.clone()).await.unwrap();
    assert_eq!(s.store.host_shutdown().unwrap().unwrap(), before);
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    s.request("admin".into(), retry).await.unwrap();
    assert_eq!(b.drains.load(Ordering::SeqCst), 2);
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn new_kernel_boot_finishes_shutdown_and_resumes_held_running_vms_once() {
    let (_dir, s, b, id) = fixture().await;
    let limits = s.store.limits().clone();
    *b.policy.lock().unwrap() = Some(HostReservePolicy {
        excluded_network_slots: vec![],
        envelope: limits.capacity,
        core_groups: limits.core_groups,
        disk_overhead_gib_per_vm: 2,
        base_pool_free_bytes: 30 * GIB,
        state: HostReserves::default(),
        applying: false,
        operation_id: None,
        minimum_reserves: Resources::default(),
        arc: None,
        memory_policy: None,
        memory: Some(MemoryCapacityStatus {
            revision: 0,
            reviewed_bytes: 32 * GIB,
            safe_bytes: 32 * GIB,
            physical_bytes: 36 * GIB,
            minimum_system_bytes: 4 * GIB,
            boot_id: b.boot.lock().unwrap().clone(),
            observed_at: homecloud_store::now(),
            arc_ready: true,
            error: None,
        }),
    });
    s.reconcile_once().await.unwrap();
    let before = s.store.vm(id).unwrap();
    let operations = s.store.operations().unwrap().len();
    request(&s, Uuid::new_v4()).await;
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    assert_eq!(s.store.vm(id).unwrap().observed, "stopped");
    assert_eq!(b.live_vms.lock().unwrap().get(&id).unwrap(), "stopped");
    *b.boot.lock().unwrap() = Uuid::new_v4().to_string();
    s.reconcile_once().await.unwrap();
    let after = s.store.vm(id).unwrap();
    assert_eq!(after.observed, "running");
    assert_eq!(b.live_vms.lock().unwrap().get(&id).unwrap(), "running");
    assert_eq!(after.desired, Lifecycle::Running);
    assert_eq!(after.resources, before.resources);
    assert_eq!(after.cpu_ids, before.cpu_ids);
    assert_eq!(s.store.operations().unwrap().len(), operations + 1);
    assert_eq!(
        s.store.host_shutdown().unwrap().unwrap().phase,
        HostShutdownPhase::Finished
    );
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.operations().unwrap().len(), operations + 1);
    assert_eq!(b.powers.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recorded_missing_vms_reject_shutdown_before_any_guest_is_drained() {
    let (_dir, s, b, id) = fixture().await;
    s.store
        .observe(
            id,
            "missing".into(),
            Some("Recorded VM is missing from Incus".into()),
        )
        .unwrap();
    let error = s
        .request(
            "admin".into(),
            Request::ShutdownHost {
                idempotency_key: Uuid::new_v4(),
                expected_boot_id: s.shutdown_boot().await.unwrap(),
            },
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Resolve the missing Incus instance for devbox"));
    assert!(s.store.host_shutdown().unwrap().is_none());
    assert_eq!(b.drains.load(Ordering::SeqCst), 0);
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn review_refreshes_read_only_guest_states_without_power_or_resource_changes() {
    let (_dir, s, b, id) = fixture().await;
    let before = s.store.vm(id).unwrap();
    let op = request(&s, Uuid::new_v4()).await;
    b.fail_drain.store(true, Ordering::SeqCst);
    s.reconcile_once().await.unwrap();
    b.live_vms.lock().unwrap().insert(id, "stopped".into());
    s.reconcile_once().await.unwrap();
    let observed = s.store.vm(id).unwrap();
    assert_eq!(observed.observed, "stopped");
    assert_eq!(observed.desired, Lifecycle::Running);
    assert_eq!(observed.resources, before.resources);
    assert_eq!(s.store.host_shutdown().unwrap().unwrap().id, op.id);
    assert_eq!(b.drains.load(Ordering::SeqCst), 1);
    assert_eq!(b.powers.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_guest_that_disappears_during_shutdown_is_not_recreated_by_cancellation() {
    let (_dir, s, b, id) = fixture().await;
    let before = s.store.vm(id).unwrap();
    let op = request(&s, Uuid::new_v4()).await;
    b.live_vms.lock().unwrap().remove(&id);
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.vm(id).unwrap().observed, "missing");
    s.request(
        "admin".into(),
        Request::CancelHostShutdown {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    assert!(s.store.pending().unwrap().is_empty());
    assert_eq!(s.store.vm(id).unwrap().resources, before.resources);
    assert!(!b.live_vms.lock().unwrap().contains_key(&id));
}

#[tokio::test]
async fn poweroff_review_timer_starts_when_the_queue_acknowledgement_arrives() {
    let (_dir, s, b, _id) = fixture().await;
    request(&s, Uuid::new_v4()).await;
    b.delay_power_reply.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        s.reconcile_once().await.unwrap();
    }
    let queued_at = b.queued_reply_at.load(Ordering::SeqCst);
    assert!(
        s.store
            .host_shutdown()
            .unwrap()
            .unwrap()
            .poweroff_requested_at
            .unwrap()
            >= queued_at
    );
}
