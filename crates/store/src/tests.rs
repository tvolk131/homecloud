use super::*;
use std::sync::{Arc, Barrier};

fn fixture() -> (tempfile::TempDir, Store, User) {
    let limits = Limits {
        capacity: Resources {
            memory_bytes: 32 * GIB,
            cpu_cores: 2,
            disk_bytes: 128 * GIB,
        },
        core_groups: vec![vec![2, 8], vec![3, 9]],
        disk_overhead_gib_per_vm: 2,
    };
    fixture_with_limits(limits)
}
fn fixture_with_limits(limits: Limits) -> (tempfile::TempDir, Store, User) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.db"), limits).unwrap();
    let (user, token) = store.bootstrap("Operator").unwrap();
    store.enroll(&token, "admin-device").unwrap();
    (dir, store, user)
}
// Each contender opens the same database independently. There is no shared
// application mutex; the database must enforce admission and claim ownership.
fn race<T: Send + 'static, F: FnOnce(Store) -> T + Send + 'static>(
    store: &Store,
    jobs: Vec<F>,
) -> Vec<T> {
    let barrier = Arc::new(Barrier::new(jobs.len() + 1));
    let threads: Vec<_> = jobs
        .into_iter()
        .map(|job| {
            let other = Store::open(&*store.path, store.limits().clone()).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                job(other)
            })
        })
        .collect();
    barrier.wait();
    threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect()
}
fn race_requests(store: &Store, requests: Vec<(String, Uuid, Mutation)>) -> Vec<Result<Operation>> {
    race(
        store,
        requests
            .into_iter()
            .map(|(peer, key, mutation)| move |other: Store| other.submit(&peer, key, mutation))
            .collect(),
    )
}
fn lease_count(store: &Store, table: &str) -> i64 {
    store
        .connect()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn spec(name: &str, memory: u64) -> VmSpec {
    VmSpec {
        applications: vec![],
        name: name.into(),
        memory_gib: memory,
        cores: 1,
        disk_gib: 10,
        ssh_public_key:
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into(),
    }
}
fn grant(store: &Store, user: Uuid, memory: u64, cores: u64) {
    store
        .grant(
            "admin-device",
            user,
            Resources {
                memory_bytes: memory * GIB,
                cpu_cores: cores,
                disk_bytes: 64 * GIB,
            },
            true,
        )
        .unwrap();
}
fn create(store: &Store, peer: &str, name: &str, memory: u64) -> Operation {
    store
        .submit(
            peer,
            Uuid::new_v4(),
            Mutation::Create {
                spec: spec(name, memory),
            },
        )
        .unwrap()
}

#[test]
fn invitations_are_single_use_and_revocation_is_permanent_for_that_key() {
    let (_dir, s, admin) = fixture();
    assert!(s.bootstrap("Another admin").is_err());
    let (token, _) = s.invite_device("admin-device", admin.id, 1).unwrap();
    s.enroll(&token, "second-device").unwrap();
    assert!(s.enroll(&token, "third-device").is_err());
    s.revoke_device("admin-device", "second-device").unwrap();
    assert!(s.authenticate("second-device").is_err());
    let (fresh, _) = s.invite_device("admin-device", admin.id, 1).unwrap();
    assert!(s.enroll(&fresh, "second-device").is_err());
    assert!(s.authenticate("admin-device").is_ok());
}

#[test]
fn expired_invitations_do_not_register_a_device() {
    let (_dir, s, admin) = fixture();
    let (token, _) = s.invite_device("admin-device", admin.id, 1).unwrap();
    s.connect()
        .unwrap()
        .execute(
            "UPDATE invitations SET expires_at=0 WHERE verifier=?",
            [token_hash(&token)],
        )
        .unwrap();
    assert!(s.enroll(&token, "late").is_err());
    assert!(s.authenticate("late").is_err());
}

#[test]
fn concurrent_creates_cannot_both_consume_the_last_memory_or_cpu_budget() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 4, 2);
    let barrier = Arc::new(Barrier::new(3));
    let jobs: Vec<_> = (0..2)
        .map(|i| {
            let s = s.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                s.submit(
                    "admin-device",
                    Uuid::new_v4(),
                    Mutation::Create {
                        spec: spec(&format!("vm-{i}"), 4),
                    },
                )
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        jobs.into_iter()
            .map(|j| j.join().unwrap().is_ok())
            .filter(|ok| *ok)
            .count(),
        1
    );
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert_eq!(s.vms().unwrap().len(), 1);
}

#[test]
fn idempotent_replay_does_not_allocate_again_and_key_reuse_is_rejected() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 8, 2);
    let key = Uuid::new_v4();
    let req = Mutation::Create {
        spec: spec("dev", 4),
    };
    let first = s.submit("admin-device", key, req.clone()).unwrap();
    let second = s.submit("admin-device", key, req).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.vm_id, second.vm_id);
    assert_eq!(s.vms().unwrap().len(), 1);
    assert!(
        s.submit(
            "admin-device",
            key,
            Mutation::Create {
                spec: spec("different", 4)
            }
        )
        .is_err()
    );
}

#[test]
fn quota_reduction_does_not_reclaim_allocations_or_reissue_their_backing() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 32, 2);
    let op = create(&s, "admin-device", "large", 24);
    s.complete(op.id, "running".into(), None, false).unwrap();
    grant(&s, admin.id, 16, 2);
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 24 * GIB);
    assert!(
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Create {
                spec: spec("extra", 1)
            }
        )
        .is_err()
    );
    let other = Resources {
        memory_bytes: 12 * GIB,
        cpu_cores: 0,
        disk_bytes: 0,
    };
    assert!(
        s.invite("admin-device", "Other", Role::User, other, false, 24)
            .is_err()
    );
    let stop = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: op.vm_id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    s.complete(stop.id, "stopped".into(), None, true).unwrap();
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 0);
    assert_eq!(s.users().unwrap()[0].grant.memory_bytes, 16 * GIB);
    let del = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: op.vm_id,
                desired: Lifecycle::Deleted,
            },
        )
        .unwrap();
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 0);
    s.complete(del.id, "deleted".into(), None, true).unwrap();
    assert_eq!(s.users().unwrap()[0].committed, Resources::default());
}

#[test]
fn unfinished_host_operation_survives_database_reopen_and_holds_resources() {
    let (dir, s, admin) = fixture();
    grant(&s, admin.id, 4, 1);
    let op = create(&s, "admin-device", "dev", 4);
    s.operation_phase(op.id, "applying", None).unwrap();
    let limits = s.limits().clone();
    drop(s);
    let restored = Store::open(dir.path().join("state.db"), limits).unwrap();
    assert_eq!(restored.pending().unwrap()[0].id, op.id);
    assert_eq!(restored.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert!(
        restored
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("duplicate", 4)
                }
            )
            .is_err()
    );
    restored
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert!(restored.pending().unwrap().is_empty());
}

#[test]
fn gpu_lease_is_exclusive_and_permission_revocation_does_not_interrupt_it() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 8, 2);
    let a = create(&s, "admin-device", "a", 4);
    s.complete(a.id, "running".into(), None, false).unwrap();
    let b = create(&s, "admin-device", "b", 4);
    s.complete(b.id, "running".into(), None, false).unwrap();
    let attach = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: a.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    s.complete(attach.id, "running".into(), None, false)
        .unwrap();
    assert!(
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: b.vm_id,
                pci: Some("0000:01:00.0".into())
            }
        )
        .is_err()
    );
    s.grant("admin-device", admin.id, s.users().unwrap()[0].grant, false)
        .unwrap();
    let stop = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: a.vm_id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    s.complete(stop.id, "stopped".into(), None, true).unwrap();
    assert!(s.vm(a.vm_id).unwrap().gpus.is_empty());
    assert_eq!(
        s.vm(a.vm_id)
            .unwrap()
            .preferred_gpus
            .first()
            .map(String::as_str),
        Some("0000:01:00.0")
    );
    let detach = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: a.vm_id,
                pci: None,
            },
        )
        .unwrap();
    s.complete(detach.id, "stopped".into(), None, true).unwrap();
    s.grant("admin-device", admin.id, s.users().unwrap()[0].grant, true)
        .unwrap();
    assert!(
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: b.vm_id,
                pci: Some("0000:01:00.0".into())
            }
        )
        .is_ok()
    );
}

#[test]
fn ordinary_users_cannot_access_other_users_vms_or_modify_grants() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 4, 1);
    let op = create(&s, "admin-device", "private", 4);
    let (token, _, _) = s
        .invite(
            "admin-device",
            "Coworker",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.enroll(&token, "coworker").unwrap();
    assert!(s.owned_vm("coworker", op.vm_id).is_err());
    assert!(
        s.submit(
            "coworker",
            Uuid::new_v4(),
            edit_request(op.vm_id, edit_spec(&s.vm(op.vm_id).unwrap()), false)
        )
        .is_err()
    );
    assert!(
        s.grant("coworker", admin.id, Resources::default(), false)
            .is_err()
    );
}

#[test]
fn independent_connections_compete_for_each_resource_without_partial_claims() {
    for quota in [
        Resources {
            memory_bytes: 4 * GIB,
            cpu_cores: 2,
            disk_bytes: 64 * GIB,
        },
        Resources {
            memory_bytes: 32 * GIB,
            cpu_cores: 1,
            disk_bytes: 64 * GIB,
        },
        Resources {
            memory_bytes: 32 * GIB,
            cpu_cores: 2,
            disk_bytes: 12 * GIB,
        },
    ] {
        let (_dir, store, admin) = fixture();
        store.grant("admin-device", admin.id, quota, true).unwrap();
        let results = race_requests(
            &store,
            (0..8)
                .map(|i| {
                    (
                        "admin-device".into(),
                        Uuid::new_v4(),
                        Mutation::Create {
                            spec: spec(&format!("race-{i}"), 4),
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{quota:?}");
        for error in results.into_iter().filter_map(Result::err) {
            assert!(error.to_string().contains("quota exceeded"), "{error:#}");
        }
        assert_eq!(
            store.users().unwrap()[0].committed,
            spec("unused", 4).resources(2).unwrap()
        );
        assert_eq!(store.vms().unwrap().len(), 1);
        assert_eq!(store.operations().unwrap().len(), 1);
        assert_eq!(lease_count(&store, "cpu_leases"), 2); // both SMT siblings
        assert_eq!(lease_count(&store, "network_leases"), 1);
    }
}

#[test]
fn concurrent_resizes_hold_the_larger_commitment_until_confirmation() {
    for dimension in ["memory", "cpu", "disk"] {
        let (_dir, store, admin) = fixture_with_limits(Limits {
            capacity: Resources {
                memory_bytes: 32 * GIB,
                cpu_cores: 4,
                disk_bytes: 128 * GIB,
            },
            core_groups: vec![vec![0, 4], vec![1, 5], vec![2, 6], vec![3, 7]],
            disk_overhead_gib_per_vm: 2,
        });
        let quota = Resources {
            memory_bytes: if dimension == "memory" {
                6 * GIB
            } else {
                32 * GIB
            },
            cpu_cores: if dimension == "cpu" { 3 } else { 4 },
            disk_bytes: if dimension == "disk" {
                36 * GIB
            } else {
                128 * GIB
            },
        };
        store.grant("admin-device", admin.id, quota, true).unwrap();
        let a = create(&store, "admin-device", "a", 2);
        let b = create(&store, "admin-device", "b", 2);
        for op in [&a, &b] {
            store
                .complete(op.id, "running".into(), None, false)
                .unwrap();
        }
        let results = race_requests(
            &store,
            [a.vm_id, b.vm_id]
                .into_iter()
                .map(|vm_id| {
                    (
                        "admin-device".into(),
                        Uuid::new_v4(),
                        Mutation::Resize {
                            vm_id,
                            memory_gib: if dimension == "memory" { 4 } else { 2 },
                            cores: if dimension == "cpu" { 2 } else { 1 },
                            disk_gib: if dimension == "disk" { 22 } else { 10 },
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{dimension}"
        );
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let claimed = store.users().unwrap()[0].committed;
        match dimension {
            "memory" => assert_eq!(claimed.memory_bytes, 6 * GIB),
            "cpu" => assert_eq!(claimed.cpu_cores, 3),
            _ => assert_eq!(claimed.disk_bytes, 36 * GIB),
        }
        store
            .operation_phase(pending[0].id, "retrying", Some("host timeout".into()))
            .unwrap();
        assert_eq!(store.users().unwrap()[0].committed, claimed);
        store
            .complete(pending[0].id, "running".into(), None, false)
            .unwrap();
        assert_eq!(store.users().unwrap()[0].committed, claimed);
    }
}

#[test]
fn simultaneous_creates_across_users_assign_distinct_complete_physical_cores() {
    let (_dir, store, _) = fixture_with_limits(Limits {
        capacity: Resources {
            memory_bytes: 16 * GIB,
            cpu_cores: 8,
            disk_bytes: 96 * GIB,
        },
        core_groups: (0..8).map(|i| vec![i, i + 8]).collect(),
        disk_overhead_gib_per_vm: 2,
    });
    let quota = Resources {
        memory_bytes: 2 * GIB,
        cpu_cores: 1,
        disk_bytes: 12 * GIB,
    };
    let mut requests = vec![];
    for i in 0..8 {
        let peer = format!("user-{i}");
        let (token, _, _) = store
            .invite("admin-device", &peer, Role::User, quota, false, 1)
            .unwrap();
        store.enroll(&token, &peer).unwrap();
        for j in 0..2 {
            requests.push((
                peer.clone(),
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec(&format!("dev-{i}-{j}"), 2),
                },
            ));
        }
    }
    let results = race_requests(&store, requests);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 8);
    let vms = store.vms().unwrap();
    assert_eq!(vms.len(), 8);
    let mut claimed = std::collections::HashSet::new();
    for vm in &vms {
        assert_eq!(vm.cpu_ids.len(), 2);
        assert!(store.limits().core_groups.contains(&vm.cpu_ids));
        for id in &vm.cpu_ids {
            assert!(claimed.insert(*id), "Physical CPU {id} allocated twice");
        }
    }
    assert_eq!(lease_count(&store, "cpu_leases"), 16);
    let users = store.users().unwrap();
    let total = users
        .into_iter()
        .filter(|u| u.role == Role::User)
        .try_fold(Resources::default(), |total, user| {
            assert_eq!(user.committed, quota);
            total.checked_add(user.committed)
        })
        .unwrap();
    assert_eq!(total, store.limits().capacity);
}

#[test]
fn concurrent_user_grants_cannot_promise_the_same_host_capacity() {
    for dimension in ["memory", "cpu", "disk"] {
        let (_dir, store, _) = fixture();
        let users: Vec<_> = (0..8)
            .map(|i| {
                store
                    .invite(
                        "admin-device",
                        &format!("user-{i}"),
                        Role::User,
                        Resources::default(),
                        false,
                        1,
                    )
                    .unwrap()
                    .1
            })
            .collect();
        let quota = Resources {
            memory_bytes: if dimension == "memory" { 32 * GIB } else { 0 },
            cpu_cores: if dimension == "cpu" { 2 } else { 0 },
            disk_bytes: if dimension == "disk" { 128 * GIB } else { 0 },
        };
        let results = race(
            &store,
            users
                .into_iter()
                .map(|user| move |other: Store| other.grant("admin-device", user, quota, false))
                .collect(),
        );
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{dimension}"
        );
        let total = store
            .users()
            .unwrap()
            .into_iter()
            .try_fold(Resources::default(), |total, user| {
                total.checked_add(user.grant)
            })
            .unwrap();
        assert_eq!(total, quota);
    }
}

#[test]
fn two_users_cannot_claim_one_gpu_even_with_different_address_case() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let (token, _, _) = store
        .invite(
            "admin-device",
            "Coworker",
            Role::User,
            Resources {
                memory_bytes: 4 * GIB,
                cpu_cores: 1,
                disk_bytes: 64 * GIB,
            },
            true,
            1,
        )
        .unwrap();
    store.enroll(&token, "coworker").unwrap();
    let a = create(&store, "admin-device", "a", 4);
    let b = create(&store, "coworker", "b", 4);
    for op in [&a, &b] {
        store
            .complete(op.id, "running".into(), None, false)
            .unwrap();
    }
    let requests = [
        ("admin-device", a.vm_id, "0000:0A:00.0"),
        ("coworker", b.vm_id, "0000:0a:00.0"),
    ];
    let results = race_requests(
        &store,
        requests
            .into_iter()
            .map(|(peer, vm_id, pci)| {
                (
                    peer.into(),
                    Uuid::new_v4(),
                    Mutation::Gpu {
                        vm_id,
                        pci: Some(pci.into()),
                    },
                )
            })
            .collect(),
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    let op = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        store.vm(op.vm_id).unwrap().gpus.first().map(String::as_str),
        Some("0000:0a:00.0")
    );
    // Uncertain host work and a daemon restart cannot make this GPU available.
    store
        .operation_phase(op.id, "retrying", Some("timeout".into()))
        .unwrap();
    let restored = Store::open(&*store.path, store.limits().clone()).unwrap();
    let loser = if op.vm_id == a.vm_id {
        ("coworker", b.vm_id)
    } else {
        ("admin-device", a.vm_id)
    };
    assert!(
        restored
            .submit(
                loser.0,
                Uuid::new_v4(),
                Mutation::Gpu {
                    vm_id: loser.1,
                    pci: Some("0000:0A:00.0".into())
                }
            )
            .is_err()
    );
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    let detach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: op.vm_id,
                pci: None,
            },
        )
        .unwrap();
    assert!(
        restored
            .submit(
                loser.0,
                Uuid::new_v4(),
                Mutation::Gpu {
                    vm_id: loser.1,
                    pci: Some("0000:0a:00.0".into())
                }
            )
            .is_err()
    );
    store
        .complete(detach.id, "running".into(), None, false)
        .unwrap();
    restored
        .submit(
            loser.0,
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: loser.1,
                pci: Some("0000:0A:00.0".into()),
            },
        )
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
}

#[test]
fn concurrent_idempotent_retries_create_one_operation_and_allocation() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let key = Uuid::new_v4();
    let results = race_requests(
        &store,
        (0..8)
            .map(|_| {
                (
                    "admin-device".into(),
                    key,
                    Mutation::Create {
                        spec: spec("dev", 4),
                    },
                )
            })
            .collect(),
    );
    let ids: std::collections::HashSet<_> = results.into_iter().map(|r| r.unwrap().id).collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(store.vms().unwrap().len(), 1);
    assert_eq!(store.operations().unwrap().len(), 1);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
}

#[test]
fn an_aborted_admission_transaction_leaves_no_resource_claim_or_intent() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let conn = store.connect().unwrap();
    // Fail after VM/CPU/network claims have been written, before operation commit.
    conn.execute_batch("CREATE TRIGGER fail_operation BEFORE INSERT ON operations BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    let key = Uuid::new_v4();
    let request = Mutation::Create {
        spec: spec("dev", 4),
    };
    assert!(store.submit("admin-device", key, request.clone()).is_err());
    assert!(store.vms().unwrap().is_empty());
    assert!(store.operations().unwrap().is_empty());
    assert_eq!(store.users().unwrap()[0].committed, Resources::default());
    for table in ["cpu_leases", "gpu_leases", "network_leases"] {
        assert_eq!(lease_count(&store, table), 0);
    }
    conn.execute_batch("DROP TRIGGER fail_operation").unwrap();
    store.submit("admin-device", key, request).unwrap();
}

#[test]
fn late_completion_cannot_release_a_newer_operations_claims() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let mut large = spec("dev", 8);
    large.cores = 2;
    let old = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Create { spec: large },
        )
        .unwrap();
    store
        .complete(old.id, "running".into(), Some("original key".into()), false)
        .unwrap();
    let shrink = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: old.vm_id,
                memory_gib: 4,
                cores: 1,
                disk_gib: 10,
            },
        )
        .unwrap();
    store
        .complete(old.id, "stopped".into(), Some("stale key".into()), true)
        .unwrap();
    assert_eq!(store.vm(old.vm_id).unwrap().observed, "running");
    assert_eq!(
        store.vm(old.vm_id).unwrap().ssh_host_key.as_deref(),
        Some("original key")
    );
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 8 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 4);
    assert!(
        store
            .operation_phase(old.id, "retrying", Some("late failure".into()))
            .is_err()
    );
    assert!(store.prepare_apply(old.id).unwrap().is_none());
    assert!(
        store
            .complete(shrink.id, "stopped".into(), None, true)
            .is_err()
    );
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("blocked", 4)
                }
            )
            .is_err()
    );
    store
        .complete(shrink.id, "running".into(), None, false)
        .unwrap();
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
    create(&store, "admin-device", "now-admitted", 4);
}

#[test]
fn simultaneous_starts_reacquire_compute_once_per_vm() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let a = create(&store, "admin-device", "a", 4);
    let b = create(&store, "admin-device", "b", 4);
    for op in [&a, &b] {
        store
            .complete(op.id, "running".into(), None, false)
            .unwrap();
        let stop = store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::State {
                    vm_id: op.vm_id,
                    desired: Lifecycle::Stopped,
                },
            )
            .unwrap();
        store
            .complete(stop.id, "stopped".into(), None, true)
            .unwrap();
    }
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 0);
    assert_eq!(lease_count(&store, "cpu_leases"), 0);
    let results = race_requests(
        &store,
        [a.vm_id, b.vm_id, a.vm_id, b.vm_id]
            .into_iter()
            .map(|vm_id| {
                (
                    "admin-device".into(),
                    Uuid::new_v4(),
                    Mutation::State {
                        vm_id,
                        desired: Lifecycle::Running,
                    },
                )
            })
            .collect(),
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 2);
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 8 * GIB);
    assert_eq!(store.users().unwrap()[0].committed.cpu_cores, 2);
    assert_eq!(lease_count(&store, "cpu_leases"), 4);
    assert_eq!(store.pending().unwrap().len(), 2);
}

#[test]
fn passive_observation_cannot_free_resources_or_bypass_manual_review() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let op = create(&store, "admin-device", "dev", 4);
    store
        .operation_phase(op.id, "needs_review", Some("ambiguous effect".into()))
        .unwrap();
    assert!(
        store
            .complete(op.id, "running".into(), None, false)
            .is_err()
    );
    assert!(store.prepare_apply(op.id).unwrap().is_none());
    store.retry_operation("admin-device", op.id).unwrap();
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert!(store.observe(op.vm_id, "deleted".into(), None).is_err());
    store
        .observe(op.vm_id, "missing".into(), Some("external removal".into()))
        .unwrap();
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
}

#[test]
fn controller_lock_excludes_another_process_and_releases_on_exit() {
    const DB: &str = "HOMECLOUD_TEST_CONTROLLER_DB";
    if let Some(path) = std::env::var_os(DB) {
        let limits: Limits =
            serde_json::from_str(&std::env::var("HOMECLOUD_TEST_CONTROLLER_LIMITS").unwrap())
                .unwrap();
        let store = Store::open(path, limits).unwrap();
        let should_lock = std::env::var("HOMECLOUD_TEST_CONTROLLER_AVAILABLE").unwrap() == "yes";
        assert_eq!(store.lock_controller().is_ok(), should_lock);
        return;
    }
    let (_dir, store, _) = fixture();
    let lock = store.lock_controller().unwrap();
    let run_child = |available: &str| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::controller_lock_excludes_another_process_and_releases_on_exit",
            ])
            .env(DB, &*store.path)
            .env(
                "HOMECLOUD_TEST_CONTROLLER_LIMITS",
                serde_json::to_string(store.limits()).unwrap(),
            )
            .env("HOMECLOUD_TEST_CONTROLLER_AVAILABLE", available)
            .output()
            .unwrap()
    };
    let child = run_child("no");
    assert!(
        child.status.success(),
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    drop(lock);
    let child = run_child("yes");
    assert!(
        child.status.success(),
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    store.lock_controller().unwrap(); // the child's lock was released on exit
}

#[test]
fn apply_context_includes_current_grants_and_pending_commitments() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let a = create(&store, "admin-device", "a", 4);
    grant(&store, admin.id, 8, 2);
    let b = create(&store, "admin-device", "b", 4);
    let context = store.prepare_apply(a.id).unwrap().unwrap();
    assert_eq!(context.operation.phase, "applying");
    assert_eq!(context.vm.id, a.vm_id);
    assert_eq!(context.vms.len(), 2);
    assert!(context.vms.iter().any(|vm| vm.id == b.vm_id));
    assert_eq!(context.users[0].committed.memory_bytes, 8 * GIB);
    assert_eq!(context.users[0].grant.memory_bytes, 8 * GIB);
}

fn request_stop(store: &Store, peer: &str, vm_id: Uuid) -> Operation {
    store
        .submit(
            peer,
            Uuid::new_v4(),
            Mutation::State {
                vm_id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap()
}
fn stopped_vm(store: &Store, peer: &str, name: &str, memory: u64) -> Uuid {
    let create = create(store, peer, name, memory);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let stop = request_stop(store, peer, create.vm_id);
    store
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    create.vm_id
}
fn prefer_gpu(store: &Store, peer: &str, vm_id: Uuid) {
    let op = store
        .submit(
            peer,
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store.complete(op.id, "stopped".into(), None, true).unwrap();
}

#[test]
fn stopping_holds_claims_through_timeout_and_reopen_until_cleanup_is_confirmed() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let create = create(&store, "admin-device", "dev", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: create.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let before = store.users().unwrap()[0].committed;
    let stop = request_stop(&store, "admin-device", create.vm_id);
    store
        .operation_phase(stop.id, "retrying", Some("GPU cleanup timed out".into()))
        .unwrap();
    let reopened = Store::open(&*store.path, store.limits().clone()).unwrap();
    assert_eq!(reopened.users().unwrap()[0].committed, before);
    assert_eq!(lease_count(&reopened, "cpu_leases"), 2);
    assert_eq!(lease_count(&reopened, "gpu_leases"), 1);
    assert!(
        reopened
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("blocked", 1)
                }
            )
            .is_err()
    );
    assert!(
        reopened
            .complete(stop.id, "running".into(), None, false)
            .is_err()
    );
    assert!(
        reopened
            .complete(stop.id, "stopped".into(), None, false)
            .is_err()
    );
    assert_eq!(lease_count(&reopened, "gpu_leases"), 1);
    reopened
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    let vm = reopened.vm(create.vm_id).unwrap();
    assert_eq!(vm.resources, before.disk_only());
    assert!(vm.cpu_ids.is_empty() && vm.gpus.is_empty());
    assert!(!vm.preferred_gpus.is_empty());
    assert_eq!(lease_count(&reopened, "cpu_leases"), 0);
    assert_eq!(lease_count(&reopened, "gpu_leases"), 0);
    assert_eq!(lease_count(&reopened, "network_leases"), 1);
    assert_eq!(reopened.users().unwrap()[0].grant.memory_bytes, 4 * GIB);
    // Releasing a VM doesn't reissue its owner's guaranteed entitlement.
    assert!(
        reopened
            .invite(
                "admin-device",
                "oversold",
                Role::User,
                Resources {
                    memory_bytes: 29 * GIB,
                    cpu_cores: 0,
                    disk_bytes: 0
                },
                false,
                1
            )
            .is_err()
    );
}

#[test]
fn competing_starts_claim_gpu_cpu_and_ram_together_or_nothing() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let a = stopped_vm(&store, "admin-device", "a", 4);
    let b = stopped_vm(&store, "admin-device", "b", 4);
    for vm in [a, b] {
        prefer_gpu(&store, "admin-device", vm);
    }
    assert_eq!(lease_count(&store, "gpu_leases"), 0); // preferences aren't leases
    let results = race_requests(
        &store,
        [a, b]
            .into_iter()
            .map(|vm_id| {
                (
                    "admin-device".into(),
                    Uuid::new_v4(),
                    Mutation::Start {
                        vm_id,
                        without_gpu: false,
                    },
                )
            })
            .collect(),
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = results
        .iter()
        .find_map(|r| r.as_ref().ok())
        .unwrap()
        .clone();
    let conflict = results
        .iter()
        .find_map(|r| r.as_ref().err())
        .unwrap()
        .downcast_ref::<GpuUnavailable>()
        .unwrap();
    let loser = conflict.vm_id;
    let vm = store.vm(loser).unwrap();
    assert_eq!(vm.desired, Lifecycle::Stopped);
    assert!(!vm.has_compute() && vm.cpu_ids.is_empty() && vm.gpus.is_empty());
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    assert_eq!(store.pending().unwrap().len(), 1);
    let cpu_only = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: loser,
                without_gpu: true,
            },
        )
        .unwrap();
    for op in [&winner, &cpu_only] {
        store
            .complete(op.id, "running".into(), None, false)
            .unwrap();
    }
    let vm = store.vm(loser).unwrap();
    assert!(vm.gpus.is_empty() && !vm.preferred_gpus.is_empty());
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 8 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 4);
    let stop = request_stop(&store, "admin-device", winner.vm_id);
    store
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
    // Repeating start on an already-running CPU-only boot doesn't reattach it.
    let noop = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: loser,
                without_gpu: false,
            },
        )
        .unwrap();
    assert!(store.vm(loser).unwrap().gpus.is_empty());
    assert_eq!(noop.phase, "succeeded");
    assert!(store.pending().unwrap().is_empty());
    let stop = request_stop(&store, "admin-device", loser);
    store
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    let next = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: loser,
                without_gpu: false,
            },
        )
        .unwrap();
    assert!(!store.vm(loser).unwrap().gpus.is_empty());
    assert_eq!(store.pending().unwrap()[0].id, next.id);
}

#[test]
fn repeated_running_intent_never_enqueues_a_launch_during_guest_reboot() {
    let (_dir, store, user) = fixture();
    grant(&store, user.id, 8, 2);
    let create = create(&store, "admin-device", "guest-reboot", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    // The host can briefly observe stopped while Incus performs its internal
    // restart. Compute remains claimed and desired state is still Running.
    store
        .observe(
            create.vm_id,
            "stopped".into(),
            Some("Guest agent reconnecting".into()),
        )
        .unwrap();
    let before = store.vm(create.vm_id).unwrap();
    let committed = store.users().unwrap()[0].committed;
    let requests = vec![
        (
            "admin-device".into(),
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: create.vm_id,
                without_gpu: false,
            },
        ),
        (
            "admin-device".into(),
            Uuid::new_v4(),
            Mutation::State {
                vm_id: create.vm_id,
                desired: Lifecycle::Running,
            },
        ),
    ];
    let results = race_requests(&store, requests);
    for result in results {
        assert_eq!(result.unwrap().phase, "succeeded");
    }
    assert!(store.pending().unwrap().is_empty());
    assert_eq!(
        json(&store.vm(create.vm_id).unwrap()).unwrap(),
        json(&before).unwrap()
    );
    assert_eq!(store.users().unwrap()[0].committed, committed);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
}

#[test]
fn repeated_running_intent_is_durably_idempotent_and_does_not_hide_errors() {
    let (_dir, store, user) = fixture();
    grant(&store, user.id, 8, 2);
    let create = create(&store, "admin-device", "already-running", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    store
        .observe(
            create.vm_id,
            "running".into(),
            Some("Agent unavailable; inspect this VM".into()),
        )
        .unwrap();
    let key = Uuid::new_v4();
    let request = Mutation::Start {
        vm_id: create.vm_id,
        without_gpu: false,
    };
    let first = store.submit("admin-device", key, request.clone()).unwrap();
    let reopened = Store::open(&*store.path, store.limits().clone()).unwrap();
    let replay = reopened.submit("admin-device", key, request).unwrap();
    assert_eq!(first.id, replay.id);
    assert_eq!(replay.phase, "succeeded");
    assert!(reopened.pending().unwrap().is_empty());
    assert_eq!(
        reopened.vm(create.vm_id).unwrap().error.as_deref(),
        Some("Agent unavailable; inspect this VM")
    );
    assert!(
        reopened
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: create.vm_id,
                    without_gpu: true
                }
            )
            .is_err()
    );
}

#[test]
fn unchanged_legacy_resize_and_empty_gpu_detach_do_not_enqueue_work_during_reboot() {
    let (_dir, store, user) = fixture();
    grant(&store, user.id, 8, 2);
    let create = create(&store, "admin-device", "reboot", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    store
        .observe(
            create.vm_id,
            "stopped".into(),
            Some("Guest reboot in progress".into()),
        )
        .unwrap();
    let before = json(&store.vm(create.vm_id).unwrap()).unwrap();
    for mutation in [
        Mutation::Resize {
            vm_id: create.vm_id,
            memory_gib: 4,
            cores: 1,
            disk_gib: 10,
        },
        Mutation::Gpu {
            vm_id: create.vm_id,
            pci: None,
        },
    ] {
        let op = store
            .submit("admin-device", Uuid::new_v4(), mutation)
            .unwrap();
        assert_eq!(op.phase, "succeeded");
        assert!(store.pending().unwrap().is_empty());
        assert_eq!(json(&store.vm(create.vm_id).unwrap()).unwrap(), before);
    }
}

#[test]
fn stopped_gpu_preferences_and_forgetting_a_cpu_only_preference_are_local() {
    let (_dir, store, user) = fixture();
    grant(&store, user.id, 8, 2);
    let id = stopped_vm(&store, "admin-device", "preferences", 4);
    let preference = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    assert_eq!(preference.phase, "succeeded");
    assert!(store.pending().unwrap().is_empty());
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
    let start = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: id,
                without_gpu: true,
            },
        )
        .unwrap();
    store
        .complete(start.id, "running".into(), None, false)
        .unwrap();
    let detach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: id,
                pci: None,
            },
        )
        .unwrap();
    assert_eq!(detach.phase, "succeeded");
    let vm = store.vm(id).unwrap();
    assert!(vm.gpus.is_empty() && vm.preferred_gpus.is_empty() && vm.has_compute());
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
    assert!(store.pending().unwrap().is_empty());
}

#[test]
fn mixed_button_spam_cannot_replace_an_unfinished_resource_change() {
    let (_dir, store, user) = fixture();
    grant(&store, user.id, 16, 2);
    let create = create(&store, "admin-device", "busy", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let active = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: create.vm_id,
                memory_gib: 6,
                cores: 1,
                disk_gib: 10,
            },
        )
        .unwrap();
    let before = json(&store.vm(create.vm_id).unwrap()).unwrap();
    let mut changes = edit_spec(&store.vm(create.vm_id).unwrap());
    changes.name = "stale edit".into();
    let variants = vec![
        Mutation::Start {
            vm_id: create.vm_id,
            without_gpu: false,
        },
        Mutation::State {
            vm_id: create.vm_id,
            desired: Lifecycle::Stopped,
        },
        Mutation::State {
            vm_id: create.vm_id,
            desired: Lifecycle::Deleted,
        },
        Mutation::ForceStop {
            vm_id: create.vm_id,
        },
        Mutation::Resize {
            vm_id: create.vm_id,
            memory_gib: 8,
            cores: 2,
            disk_gib: 12,
        },
        Mutation::Gpu {
            vm_id: create.vm_id,
            pci: None,
        },
        Mutation::Edit {
            vm_id: create.vm_id,
            changes,
            restart_confirmed: true,
        },
    ];
    let results = race_requests(
        &store,
        variants
            .into_iter()
            .cycle()
            .take(28)
            .map(|m| ("admin-device".into(), Uuid::new_v4(), m))
            .collect(),
    );
    assert!(results.iter().all(|r| {
        r.as_ref()
            .err()
            .unwrap()
            .to_string()
            .contains("unfinished operation")
    }));
    assert_eq!(store.pending().unwrap().len(), 1);
    assert_eq!(store.pending().unwrap()[0].id, active.id);
    assert_eq!(json(&store.vm(create.vm_id).unwrap()).unwrap(), before);
    assert!(store.retry_operation("admin-device", active.id).is_err());
}

#[test]
fn cpu_only_consent_rechecks_current_quota_and_gpu_permission_is_prospective() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let vm = stopped_vm(&store, "admin-device", "dev", 4);
    prefer_gpu(&store, "admin-device", vm);
    let grant = store.users().unwrap()[0].grant;
    store.grant("admin-device", admin.id, grant, false).unwrap();
    let error = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: vm,
                without_gpu: false,
            },
        )
        .unwrap_err();
    assert!(error.downcast_ref::<GpuUnavailable>().is_some());
    assert_eq!(lease_count(&store, "cpu_leases"), 0);
    // Quota can change while the conflict dialog is open. Consent bypasses
    // neither current admission nor the configured guest memory requirement.
    store
        .grant(
            "admin-device",
            admin.id,
            Resources {
                memory_bytes: 2 * GIB,
                ..grant
            },
            false,
        )
        .unwrap();
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: vm,
                    without_gpu: true
                }
            )
            .is_err()
    );
    assert!(!store.vm(vm).unwrap().has_compute());
    store.grant("admin-device", admin.id, grant, false).unwrap();
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: vm,
                without_gpu: true,
            },
        )
        .unwrap();
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert!(!store.vm(vm).unwrap().preferred_gpus.is_empty());
    assert!(store.vm(vm).unwrap().gpus.is_empty());
}

#[test]
fn start_versus_create_competes_for_the_same_fungible_capacity() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let vm = stopped_vm(&store, "admin-device", "old", 4);
    let results = race_requests(
        &store,
        vec![
            (
                "admin-device".into(),
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: vm,
                    without_gpu: false,
                },
            ),
            (
                "admin-device".into(),
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("new", 4),
                },
            ),
        ],
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 4 * GIB);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
}

#[test]
fn internal_resize_restart_keeps_claims_but_stopped_resize_claims_disk_only() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let create = create(&store, "admin-device", "dev", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: create.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let resize = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: create.vm_id,
                memory_gib: 6,
                cores: 2,
                disk_gib: 10,
            },
        )
        .unwrap();
    store.observe(create.vm_id, "stopped".into(), None).unwrap(); // internal interruption
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    assert_eq!(lease_count(&store, "cpu_leases"), 4);
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 6 * GIB);
    assert!(
        store
            .complete(resize.id, "stopped".into(), None, true)
            .is_err()
    );
    store
        .complete(resize.id, "running".into(), None, false)
        .unwrap();
    let stop = request_stop(&store, "admin-device", create.vm_id);
    store
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    let resize = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: create.vm_id,
                memory_gib: 16,
                cores: 4,
                disk_gib: 20,
            },
        )
        .unwrap();
    store
        .complete(resize.id, "stopped".into(), None, true)
        .unwrap();
    let vm = store.vm(create.vm_id).unwrap();
    assert_eq!(vm.spec.memory_gib, 16);
    assert_eq!(vm.resources.disk_bytes, 22 * GIB);
    assert!(!vm.has_compute() && vm.gpus.is_empty() && vm.cpu_ids.is_empty());
    assert!(!vm.preferred_gpus.is_empty());
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: vm.id,
                    without_gpu: false
                }
            )
            .is_err()
    );
}

#[test]
fn old_stopped_records_keep_claims_and_gain_gpu_preference_until_explicit_cleanup() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let create = create(&store, "admin-device", "legacy", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: create.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let mut legacy = serde_json::to_value(store.vm(create.vm_id).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("preferred_gpu");
    legacy.as_object_mut().unwrap().remove("preferred_gpus");
    legacy["desired"] = serde_json::json!("stopped");
    legacy["observed"] = serde_json::json!("stopped");
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE vms SET record=? WHERE id=?",
            params![legacy.to_string(), create.vm_id.to_string()],
        )
        .unwrap();
    let reopened = Store::open(&*store.path, store.limits().clone()).unwrap();
    let vm = reopened.vm(create.vm_id).unwrap();
    assert!(vm.has_compute() && !vm.gpus.is_empty());
    assert_eq!(vm.preferred_gpus, vm.gpus);
    let stop = request_stop(&reopened, "admin-device", vm.id);
    reopened
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    assert!(!reopened.vm(vm.id).unwrap().has_compute());
}

#[test]
fn cpu_only_start_intent_survives_reopen_and_late_stop_completion() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let created = create(&store, "admin-device", "dev", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: created.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let stopped = request_stop(&store, "admin-device", created.vm_id);
    store
        .complete(stopped.id, "stopped".into(), None, true)
        .unwrap();
    let start = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: created.vm_id,
                without_gpu: true,
            },
        )
        .unwrap();
    store.operation_phase(start.id, "applying", None).unwrap();
    let restored = Store::open(&*store.path, store.limits().clone()).unwrap();
    assert!(matches!(
        restored.pending().unwrap()[0].request,
        Mutation::Start {
            without_gpu: true,
            ..
        }
    ));
    restored
        .complete(stopped.id, "stopped".into(), None, true)
        .unwrap(); // delayed duplicate
    let vm = restored.vm(created.vm_id).unwrap();
    assert!(vm.has_compute() && vm.gpus.is_empty() && !vm.preferred_gpus.is_empty());
    assert_eq!(lease_count(&restored, "cpu_leases"), 2);
    assert!(
        restored
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("blocked", 4)
                }
            )
            .is_err()
    );
    restored
        .complete(start.id, "running".into(), None, false)
        .unwrap();
    assert!(restored.vm(created.vm_id).unwrap().gpus.is_empty());
}

fn app_fixture() -> (tempfile::TempDir, Store, User, Uuid, ManagedApplication) {
    let (dir, s, user) = fixture();
    grant(&s, user.id, 8, 2);
    let mut wanted = spec("managed", 2);
    wanted.applications = vec![ApplicationKind::T3];
    let op = s
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Create { spec: wanted },
        )
        .unwrap();
    s.complete(op.id, "running".into(), None, false).unwrap();
    let app = s.applications().unwrap().remove(0);
    (dir, s, user, op.vm_id, app)
}

#[test]
fn application_install_is_atomic_with_vm_creation_and_does_not_hold_vm_operation_lane() {
    let (_dir, s, _user, vm, app) = app_fixture();
    assert_eq!(app.vm_id, vm);
    assert_eq!(s.application_operations().unwrap().len(), 1);
    assert!(
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: vm,
                desired: Lifecycle::Stopped
            }
        )
        .is_ok()
    );
    let mut invalid = spec("quota-failure", 64);
    invalid.applications = vec![ApplicationKind::T3];
    assert!(
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Create { spec: invalid }
        )
        .is_err()
    );
    assert_eq!(s.applications().unwrap().len(), 1);
}

#[test]
fn concurrent_application_requests_reuse_one_durable_job_and_enforce_ownership() {
    let (_dir, s, _user, vm, app) = app_fixture();
    let results = race(
        &s,
        (0..6)
            .map(|_| {
                move |other: Store| {
                    other
                        .ensure_application("admin-device", vm, ApplicationKind::T3, Uuid::new_v4())
                        .unwrap()
                        .id
                }
            })
            .collect(),
    );
    assert!(results.iter().all(|id| *id == results[0]));
    assert_eq!(s.application_operations().unwrap().len(), 1);
    let (token, _, _) = s
        .invite(
            "admin-device",
            "other",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.enroll(&token, "other-device").unwrap();
    assert!(
        s.ensure_application("other-device", vm, ApplicationKind::T3, Uuid::new_v4())
            .is_err()
    );
    assert!(
        s.start_application_setup(
            "other-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::ConnectT3
        )
        .is_err()
    );
}

#[test]
fn application_recovery_preserves_operation_and_excludes_browser_challenges_from_database() {
    let (dir, s, _user, _vm, app) = app_fixture();
    let install = s.application_operations().unwrap().remove(0);
    let report = |id, phase, setup: &str| ApplicationReport {
        observation: ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            service: "running".into(),
            setup: setup.into(),
            action: Some(BrowserAction {
                url: "https://example.com/private-challenge".into(),
                code: "PRIVATE-CODE".into(),
                expires_at: now() + 60,
                setup_action: ApplicationSetupAction::ConnectT3,
                input_required: false,
            }),
            ..Default::default()
        },
        job: Some(ApplicationJob { id, phase }),
    };
    s.record_application_report(
        app.id,
        report(install.id, ApplicationPhase::Succeeded, "required"),
    )
    .unwrap();
    let op = s
        .start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::ConnectT3,
        )
        .unwrap();
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::WaitingForUser, "waiting_for_user"),
    )
    .unwrap();
    assert!(s.applications().unwrap()[0].observation.action.is_none());
    let reopened = Store::open(dir.path().join("state.db"), s.limits().clone()).unwrap();
    assert_eq!(
        reopened
            .start_application_setup(
                "admin-device",
                app.id,
                Uuid::new_v4(),
                ApplicationSetupAction::ConnectT3
            )
            .unwrap()
            .id,
        op.id
    );
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Interrupted, "finishing"),
    )
    .unwrap();
    s.resume_application_setup(op.id).unwrap();
    let resumed = s
        .application_operations()
        .unwrap()
        .into_iter()
        .find(|o| o.id == op.id)
        .unwrap();
    assert_eq!(resumed.phase, ApplicationPhase::Pending);
    let stored: String = s
        .connect()
        .unwrap()
        .query_row(
            "SELECT record FROM applications WHERE id=?",
            [app.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains("PRIVATE-CODE"));
    assert!(!stored.contains("private-challenge"));
}

#[test]
fn late_confirmed_setup_success_repairs_interrupted_status_without_reviving_other_work() {
    let (_dir, s, _user, vm, app) = app_fixture();
    let install = s.application_operations().unwrap().remove(0);
    let report = |id, phase, setup: &str| ApplicationReport {
        observation: ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            service: "running".into(),
            setup: setup.into(),
            ..Default::default()
        },
        job: Some(ApplicationJob { id, phase }),
    };
    s.record_application_report(
        app.id,
        report(install.id, ApplicationPhase::Succeeded, "required"),
    )
    .unwrap();
    let op = s
        .start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::ConnectT3,
        )
        .unwrap();
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Interrupted, "interrupted"),
    )
    .unwrap();
    let before = s.vm(vm).unwrap().resources;
    s.record_application_report(app.id, report(op.id, ApplicationPhase::Succeeded, "ready"))
        .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Succeeded
    );
    assert_eq!(s.vm(vm).unwrap().resources, before);
    // A stale running report cannot reopen the finished operation.
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Running, "in_progress"),
    )
    .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Succeeded
    );
    let next = s
        .start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::ConnectT3,
        )
        .unwrap();
    s.record_application_report(
        app.id,
        report(next.id, ApplicationPhase::Interrupted, "interrupted"),
    )
    .unwrap();
    s.submit(
        "admin-device",
        Uuid::new_v4(),
        Mutation::State {
            vm_id: vm,
            desired: Lifecycle::Deleted,
        },
    )
    .unwrap();
    s.record_application_report(
        app.id,
        report(next.id, ApplicationPhase::Succeeded, "ready"),
    )
    .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Interrupted
    );
}

#[test]
fn named_provider_setup_is_owner_only_atomic_and_recovers_only_its_own_success() {
    let (dir, s, _user, _vm, app) = app_fixture();
    let install = s.application_operations().unwrap().remove(0);
    let report = |id, phase, account: &str| ApplicationReport {
        observation: ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            service: "running".into(),
            setup: "ready".into(),
            components: vec![ApplicationComponent {
                name: "Codex CLI".into(),
                version: Some("0.165.0".into()),
                ready: true,
                account: account.into(),
            }],
            action: Some(BrowserAction {
                url: "https://auth.openai.com/codex/device".into(),
                code: "PRIVATE-CODEX-CODE".into(),
                expires_at: now() + 600,
                setup_action: ApplicationSetupAction::SignInCodex,
                input_required: false,
            }),
            ..Default::default()
        },
        job: Some(ApplicationJob { id, phase }),
    };
    s.record_application_report(
        app.id,
        report(install.id, ApplicationPhase::Succeeded, "signed_out"),
    )
    .unwrap();
    // Infrastructure administrators may manage installations, but cannot put
    // their provider subscription into someone else's VM through this API.
    let (token, _, _) = s
        .invite(
            "admin-device",
            "Another admin",
            Role::Admin,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.enroll(&token, "another-admin").unwrap();
    assert!(
        s.start_application_setup(
            "another-admin",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::SignInCodex
        )
        .is_err()
    );
    let results = race(
        &s,
        (0..6)
            .map(|_| {
                move |other: Store| {
                    other
                        .start_application_setup(
                            "admin-device",
                            app.id,
                            Uuid::new_v4(),
                            ApplicationSetupAction::SignInCodex,
                        )
                        .unwrap()
                        .id
                }
            })
            .collect(),
    );
    assert!(results.iter().all(|id| *id == results[0]));
    let id = results[0];
    assert!(
        s.start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::ConnectT3
        )
        .is_err()
    );
    let conn = s.connect().unwrap();
    let key: String = conn
        .query_row(
            "SELECT request_key FROM application_operations WHERE id=?",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        s.start_application_setup(
            "admin-device",
            app.id,
            key.parse().unwrap(),
            ApplicationSetupAction::ConnectT3
        )
        .is_err()
    );
    s.record_application_report(
        app.id,
        report(id, ApplicationPhase::WaitingForUser, "signed_out"),
    )
    .unwrap();
    let reopened = Store::open(dir.path().join("state.db"), s.limits().clone()).unwrap();
    assert_eq!(
        reopened
            .start_application_setup(
                "admin-device",
                app.id,
                Uuid::new_v4(),
                ApplicationSetupAction::SignInCodex
            )
            .unwrap()
            .id,
        id
    );
    s.record_application_report(
        app.id,
        report(id, ApplicationPhase::Interrupted, "signed_out"),
    )
    .unwrap();
    s.resume_application_setup(id).unwrap();
    s.record_application_report(
        app.id,
        report(id, ApplicationPhase::Succeeded, "signed_out"),
    )
    .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Interrupted
    );
    s.record_application_report(app.id, report(id, ApplicationPhase::Succeeded, "signed_in"))
        .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Succeeded
    );
    let stored: String = conn
        .query_row(
            "SELECT record FROM applications WHERE id=?",
            [app.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains("PRIVATE-CODEX-CODE"));
    assert!(!stored.contains("auth.openai.com"));
}

#[test]
fn github_access_is_owner_only_needs_the_cli_and_removal_recovers_only_when_signed_out() {
    let (_dir, s, _user, _vm, app) = app_fixture();
    let install = s.application_operations().unwrap().remove(0);
    let report = |id, phase, components: Vec<ApplicationComponent>| ApplicationReport {
        observation: ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            service: "running".into(),
            setup: "ready".into(),
            components,
            ..Default::default()
        },
        job: Some(ApplicationJob { id, phase }),
    };
    let github = |account: &str| {
        vec![ApplicationComponent {
            name: "GitHub CLI".into(),
            version: Some("2.103.3".into()),
            ready: true,
            account: account.into(),
        }]
    };
    // An older guest runner reports no GitHub CLI; neither action may start.
    s.record_application_report(
        app.id,
        report(install.id, ApplicationPhase::Succeeded, vec![]),
    )
    .unwrap();
    for action in [
        ApplicationSetupAction::SignInGitHub,
        ApplicationSetupAction::SignOutGitHub,
    ] {
        assert!(
            s.start_application_setup("admin-device", app.id, Uuid::new_v4(), action)
                .is_err()
        );
    }
    s.record_application_report(
        app.id,
        report(install.id, ApplicationPhase::Succeeded, github("signed_in")),
    )
    .unwrap();
    let (token, _, _) = s
        .invite(
            "admin-device",
            "Another admin",
            Role::Admin,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.enroll(&token, "another-admin").unwrap();
    for action in [
        ApplicationSetupAction::SignInGitHub,
        ApplicationSetupAction::SignOutGitHub,
    ] {
        assert!(
            s.start_application_setup("another-admin", app.id, Uuid::new_v4(), action)
                .is_err()
        );
    }
    let op = s
        .start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::SignOutGitHub,
        )
        .unwrap();
    // One unfinished account operation per application, whatever its target.
    assert!(
        s.start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::SignInGitHub
        )
        .is_err()
    );
    let conn = s.connect().unwrap();
    let record: String = conn
        .query_row(
            "SELECT record FROM application_operations WHERE id=?",
            [op.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(record.contains("\"sign_out_github\""));
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Interrupted, github("signed_in")),
    )
    .unwrap();
    // A late success report only counts once the account is actually gone.
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Succeeded, github("signed_in")),
    )
    .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Interrupted
    );
    s.record_application_report(
        app.id,
        report(op.id, ApplicationPhase::Succeeded, github("signed_out")),
    )
    .unwrap();
    assert_eq!(
        s.application_operations().unwrap().last().unwrap().phase,
        ApplicationPhase::Succeeded
    );
}

#[test]
fn claude_code_submission_requires_current_owner_and_exact_waiting_operation() {
    let (_dir, s, _owner, _vm, app) = app_fixture();
    let install = s.application_operations().unwrap().remove(0);
    let report = |id, phase| ApplicationReport {
        observation: ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            setup: "ready".into(),
            operation_id: Some(id),
            components: vec![ApplicationComponent {
                name: "Claude Code".into(),
                version: Some("2.1.308".into()),
                ready: true,
                account: "signed_out".into(),
            }],
            ..Default::default()
        },
        job: Some(ApplicationJob { id, phase }),
    };
    s.record_application_report(app.id, report(install.id, ApplicationPhase::Succeeded))
        .unwrap();
    let op = s
        .start_application_setup(
            "admin-device",
            app.id,
            Uuid::new_v4(),
            ApplicationSetupAction::SignInClaude,
        )
        .unwrap();
    assert!(
        s.authorize_application_code("admin-device", app.id, op.id)
            .is_err()
    );
    s.record_application_report(app.id, report(op.id, ApplicationPhase::WaitingForUser))
        .unwrap();
    assert_eq!(
        s.authorize_application_code("admin-device", app.id, op.id)
            .unwrap()
            .2
            .id,
        op.id
    );
    assert!(
        s.authorize_application_code("admin-device", app.id, install.id)
            .is_err()
    );
    let (token, _, _) = s
        .invite(
            "admin-device",
            "Other admin",
            Role::Admin,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    s.enroll(&token, "other-admin").unwrap();
    assert!(
        s.authorize_application_code("other-admin", app.id, op.id)
            .is_err()
    );
    s.record_application_report(app.id, report(op.id, ApplicationPhase::Succeeded))
        .unwrap();
    assert!(
        s.authorize_application_code("admin-device", app.id, op.id)
            .is_err()
    );
}

#[test]
fn explicit_force_stop_keeps_all_claims_until_verified_cleanup_and_survives_restart() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let created = create(&store, "admin-device", "force recovery", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: created.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let before = store.vm(created.vm_id).unwrap();
    let stop = request_stop(&store, "admin-device", created.vm_id);
    let key = Uuid::new_v4();
    let request = Mutation::ForceStop {
        vm_id: created.vm_id,
    };
    assert!(store.submit("admin-device", key, request.clone()).is_err()); // active stop cannot be raced
    store
        .operation_phase(
            stop.id,
            "needs_review",
            Some("graceful shutdown timed out".into()),
        )
        .unwrap();
    let force = store.submit("admin-device", key, request.clone()).unwrap();
    assert_eq!(
        store.submit("admin-device", key, request).unwrap().id,
        force.id
    );
    assert_eq!(store.vm(created.vm_id).unwrap().resources, before.resources);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    assert_eq!(
        store
            .operations()
            .unwrap()
            .iter()
            .find(|o| o.id == stop.id)
            .unwrap()
            .phase,
        "failed"
    );
    assert!(
        store
            .complete(stop.id, "stopped".into(), None, true)
            .is_err()
    );
    assert!(store.operation_phase(stop.id, "retrying", None).is_err());
    assert!(store.retry_operation("admin-device", stop.id).is_err());
    let reopened = Store::open(&*store.path, store.limits().clone()).unwrap();
    assert!(matches!(
        reopened
            .prepare_apply(force.id)
            .unwrap()
            .unwrap()
            .operation
            .request,
        Mutation::ForceStop { .. }
    ));
    assert!(
        reopened
            .complete(force.id, "running".into(), None, true)
            .is_err()
    );
    assert!(
        reopened
            .complete(force.id, "stopped".into(), None, false)
            .is_err()
    );
    assert_eq!(
        reopened.vm(created.vm_id).unwrap().resources,
        before.resources
    );
    reopened
        .complete(force.id, "stopped".into(), None, true)
        .unwrap();
    let after = reopened.vm(created.vm_id).unwrap();
    assert_eq!(after.resources, before.resources.disk_only());
    assert!(after.cpu_ids.is_empty() && after.gpus.is_empty());
    assert_eq!(after.preferred_gpus, before.preferred_gpus);
    assert_eq!(lease_count(&reopened, "cpu_leases"), 0);
    assert_eq!(lease_count(&reopened, "gpu_leases"), 0);
    let audited: i64 = reopened
        .connect()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM audit WHERE action='request_force_stop' AND actor='admin-device'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
}

#[test]
fn graceful_retry_and_force_stop_compete_for_one_durable_operation_lane() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let created = create(&store, "admin-device", "race", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let stop = request_stop(&store, "admin-device", created.vm_id);
    store
        .operation_phase(stop.id, "needs_review", Some("timeout".into()))
        .unwrap();
    let results = race_requests(
        &store,
        vec![
            (
                "admin-device".into(),
                Uuid::new_v4(),
                Mutation::ForceStop {
                    vm_id: created.vm_id,
                },
            ),
            (
                "admin-device".into(),
                Uuid::new_v4(),
                Mutation::State {
                    vm_id: created.vm_id,
                    desired: Lifecycle::Stopped,
                },
            ),
        ],
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(store.pending().unwrap().len(), 1);
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
    assert_eq!(
        store.vm(created.vm_id).unwrap().resources.memory_bytes,
        4 * GIB
    );
}

#[test]
fn ordinary_owner_can_force_stop_but_another_user_cannot_supersede_the_review() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let (token, _, _) = store
        .invite(
            "admin-device",
            "Other",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    store.enroll(&token, "other-device").unwrap();
    let created = create(&store, "admin-device", "owned", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let stop = request_stop(&store, "admin-device", created.vm_id);
    store
        .operation_phase(stop.id, "needs_review", Some("timeout".into()))
        .unwrap();
    store
        .connect()
        .unwrap()
        .execute(
            "UPDATE users SET role='user' WHERE id=?",
            [admin.id.to_string()],
        )
        .unwrap();
    let request = Mutation::ForceStop {
        vm_id: created.vm_id,
    };
    assert!(
        store
            .submit("other-device", Uuid::new_v4(), request.clone())
            .is_err()
    );
    assert_eq!(
        store
            .operations()
            .unwrap()
            .iter()
            .find(|o| o.id == stop.id)
            .unwrap()
            .phase,
        "needs_review"
    );
    let force = store
        .submit("admin-device", Uuid::new_v4(), request)
        .unwrap();
    assert!(force.request.stops_vm());
    assert_eq!(force.actor, admin.id);
}

#[test]
fn force_stop_cannot_replace_a_reviewed_non_stop_operation() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 4, 1);
    let create = create(&store, "admin-device", "incomplete create", 4);
    store
        .operation_phase(
            create.id,
            "needs_review",
            Some("provisioning conflict".into()),
        )
        .unwrap();
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::ForceStop {
                    vm_id: create.vm_id
                }
            )
            .is_err()
    );
    assert_eq!(lease_count(&store, "cpu_leases"), 2);
}

fn edit_spec(vm: &Vm) -> WorkspaceEdit {
    WorkspaceEdit {
        name: vm.spec.name.clone(),
        memory_gib: vm.spec.memory_gib,
        cores: vm.spec.cores,
        disk_gib: vm.spec.disk_gib,
        gpu: GpuEdit::Keep,
    }
}
fn edit_request(vm: Uuid, changes: WorkspaceEdit, restart_confirmed: bool) -> Mutation {
    Mutation::Edit {
        vm_id: vm,
        changes,
        restart_confirmed,
    }
}
#[test]
fn rename_is_a_local_audited_edit_preserving_identity_accounts_and_claims() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let mut configured = spec("before", 4);
    configured.applications = vec![ApplicationKind::T3];
    let create = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Create { spec: configured },
        )
        .unwrap();
    let application = store.applications().unwrap()[0].id;
    store
        .complete(
            create.id,
            "running".into(),
            Some("pinned-host-key".into()),
            false,
        )
        .unwrap();
    let original = store.vm(create.vm_id).unwrap();
    grant(&store, admin.id, 2, 1); // A name change still works above the new RAM quota.
    let mut changes = edit_spec(&original);
    changes.name = "after".into();
    let key = Uuid::new_v4();
    let op = store
        .submit(
            "admin-device",
            key,
            edit_request(original.id, changes.clone(), false),
        )
        .unwrap();
    assert_eq!(op.phase, "succeeded");
    assert_eq!(
        store
            .submit(
                "admin-device",
                key,
                edit_request(original.id, changes, false)
            )
            .unwrap()
            .id,
        op.id
    );
    let renamed = store.vm(original.id).unwrap();
    assert_eq!(renamed.spec.name, "after");
    assert_eq!(store.applications().unwrap()[0].id, application);
    assert_eq!(renamed.incus_name(), original.incus_name());
    assert_eq!(renamed.ssh_host_key, original.ssh_host_key);
    assert_eq!(renamed.cpu_ids, original.cpu_ids);
    assert_eq!(renamed.resources, original.resources);
    assert_eq!(renamed.network_slot, original.network_slot);
    assert!(store.prepare_apply(op.id).unwrap().is_none());
}
#[test]
fn edit_restart_consent_is_rechecked_by_the_server_before_any_claim_or_rename() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let create = create(&store, "admin-device", "original", 2);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(create.vm_id).unwrap());
    changes.name = "new-name".into();
    changes.memory_gib = 4;
    changes.gpu = GpuEdit::Set {
        pci: Some("0000:01:00.0".into()),
    };
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                edit_request(create.vm_id, changes.clone(), false)
            )
            .unwrap_err()
            .to_string()
            .contains("restart")
    );
    assert_eq!(store.vm(create.vm_id).unwrap().spec.name, "original");
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 2 * GIB);
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(create.vm_id, changes, true),
        )
        .unwrap();
    let vm = store.vm(create.vm_id).unwrap();
    assert_eq!(vm.spec.memory_gib, 4);
    assert!(!vm.pending_gpus.is_empty());
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert_eq!(
        store.vm(vm.id).unwrap().gpus.first().map(String::as_str),
        Some("0000:01:00.0")
    );
}
#[test]
fn combined_edit_holds_both_gpus_and_old_compute_until_verified_completion_after_reopen() {
    let (dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let create = create(&store, "admin-device", "swap", 8);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: create.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(create.vm_id).unwrap());
    changes.memory_gib = 4;
    changes.disk_gib = 20;
    changes.gpu = GpuEdit::Set {
        pci: Some("0000:0A:00.0".into()),
    };
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(create.vm_id, changes, true),
        )
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    store.observe(create.vm_id, "stopped".into(), None).unwrap();
    store
        .operation_phase(op.id, "needs_review", Some("GPU reset failed".into()))
        .unwrap();
    let reopened = Store::open(dir.path().join("state.db"), store.limits().clone()).unwrap();
    let held = reopened.vm(create.vm_id).unwrap();
    assert!(held.claims_gpu("0000:01:00.0"));
    assert!(held.claims_gpu("0000:0a:00.0"));
    assert_eq!(held.resources.memory_bytes, 8 * GIB);
    assert_eq!(held.resources.disk_bytes, 22 * GIB);
    assert!(
        reopened
            .complete(op.id, "running".into(), None, false)
            .is_err()
    );
    reopened.retry_operation("admin-device", op.id).unwrap();
    reopened
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    let finished = reopened.vm(create.vm_id).unwrap();
    assert_eq!(
        finished.gpus.first().map(String::as_str),
        Some("0000:0a:00.0")
    );
    assert!(finished.pending_gpus.is_empty());
    assert_eq!(lease_count(&reopened, "gpu_leases"), 1);
    assert_eq!(finished.resources.memory_bytes, 4 * GIB);
}
#[test]
fn simultaneous_combined_edits_cannot_claim_the_same_gpu_or_leave_partial_resource_changes() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let a = create(&store, "admin-device", "a", 2);
    store.complete(a.id, "running".into(), None, false).unwrap();
    let b = create(&store, "admin-device", "b", 2);
    store.complete(b.id, "running".into(), None, false).unwrap();
    let requests = [a.vm_id, b.vm_id]
        .into_iter()
        .map(|vm| {
            let mut changes = edit_spec(&store.vm(vm).unwrap());
            changes.memory_gib = 4;
            changes.disk_gib = 20;
            changes.gpu = GpuEdit::Set {
                pci: Some("0000:01:00.0".into()),
            };
            (
                "admin-device".into(),
                Uuid::new_v4(),
                edit_request(vm, changes, true),
            )
        })
        .collect();
    let results = race_requests(&store, requests);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 6 * GIB);
    let loser = results.iter().position(|r| r.is_err()).unwrap();
    let id = [a.vm_id, b.vm_id][loser];
    let vm = store.vm(id).unwrap();
    assert_eq!(vm.spec.memory_gib, 2);
    assert_eq!(vm.spec.disk_gib, 10);
    assert!(vm.pending_gpus.is_empty());
}
#[test]
fn stopped_edits_do_not_start_or_claim_compute_and_can_remember_an_occupied_gpu() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let running = create(&store, "admin-device", "owner", 4);
    store
        .complete(running.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: running.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let id = stopped_vm(&store, "admin-device", "off", 4);
    let mut changes = edit_spec(&store.vm(id).unwrap());
    changes.memory_gib = 8;
    changes.cores = 8;
    changes.disk_gib = 20;
    changes.gpu = GpuEdit::Set {
        pci: Some("0000:01:00.0".into()),
    };
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(id, changes, false),
        )
        .unwrap();
    assert_eq!(store.vm(id).unwrap().resources.cpu_cores, 0);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    store.complete(op.id, "stopped".into(), None, true).unwrap();
    assert_eq!(store.vm(id).unwrap().desired, Lifecycle::Stopped);
    assert!(store.vm(id).unwrap().gpus.is_empty());
    assert_eq!(
        store
            .vm(id)
            .unwrap()
            .preferred_gpus
            .first()
            .map(String::as_str),
        Some("0000:01:00.0")
    );
}
#[test]
fn gpu_vm_can_downsize_to_the_normal_minimum_and_detach_without_releasing_early() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let create = create(&store, "admin-device", "gpu", 4);
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: create.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(create.vm_id).unwrap());
    changes.memory_gib = 1;
    let keep = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(create.vm_id, changes.clone(), true),
        )
        .unwrap();
    assert_eq!(
        store.vm(create.vm_id).unwrap().resources.memory_bytes,
        4 * GIB
    );
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    store
        .complete(keep.id, "running".into(), None, false)
        .unwrap();
    let vm = store.vm(create.vm_id).unwrap();
    assert_eq!(vm.resources.memory_bytes, GIB);
    assert_eq!(vm.gpus, vec!["0000:01:00.0"]);
    changes.gpu = GpuEdit::Set { pci: None };
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(create.vm_id, changes, true),
        )
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    assert_eq!(store.vm(create.vm_id).unwrap().resources.memory_bytes, GIB);
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
}
#[test]
fn gpu_keep_does_not_reclaim_a_preference_when_the_vm_started_without_it() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let id = stopped_vm(&store, "admin-device", "without", 4);
    let preference = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(preference.id, "stopped".into(), None, true)
        .unwrap();
    let start = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: id,
                without_gpu: true,
            },
        )
        .unwrap();
    store
        .complete(start.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(id).unwrap());
    changes.name = "still-without".into();
    let op = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(id, changes, false),
        )
        .unwrap();
    assert_eq!(op.phase, "succeeded");
    assert!(store.vm(id).unwrap().gpus.is_empty());
    assert!(!store.vm(id).unwrap().preferred_gpus.is_empty());
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
}
#[test]
fn gpu_transition_schema_upgrade_preserves_the_old_unique_claim() {
    let (dir, store, admin) = fixture();
    grant(&store, admin.id, 8, 2);
    let op = create(&store, "admin-device", "legacy", 4);
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: op.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    store
        .connect()
        .unwrap()
        .execute_batch(
            "DELETE FROM meta WHERE key='gpu_transition_claims';
        ALTER TABLE gpu_leases RENAME TO gpu_old;
        CREATE TABLE gpu_leases(pci TEXT PRIMARY KEY,vm_id TEXT NOT NULL UNIQUE REFERENCES vms(id));
        INSERT INTO gpu_leases SELECT * FROM gpu_old;DROP TABLE gpu_old;",
        )
        .unwrap();
    let reopened = Store::open(dir.path().join("state.db"), store.limits().clone()).unwrap();
    assert_eq!(lease_count(&reopened, "gpu_leases"), 1);
    assert!(reopened.vm(op.vm_id).unwrap().claims_gpu("0000:01:00.0"));
    let mut changes = edit_spec(&reopened.vm(op.vm_id).unwrap());
    changes.gpu = GpuEdit::Set {
        pci: Some("0000:02:00.0".into()),
    };
    reopened
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(op.vm_id, changes, true),
        )
        .unwrap();
    assert_eq!(lease_count(&reopened, "gpu_leases"), 2);
}

#[test]
fn combined_edits_compete_for_each_fungible_budget_without_partial_claims() {
    for dimension in ["memory", "cpu", "disk"] {
        let (_dir, store, admin) = fixture_with_limits(Limits {
            capacity: Resources {
                memory_bytes: 32 * GIB,
                cpu_cores: 4,
                disk_bytes: 128 * GIB,
            },
            core_groups: vec![vec![0, 4], vec![1, 5], vec![2, 6], vec![3, 7]],
            disk_overhead_gib_per_vm: 2,
        });
        let quota = Resources {
            memory_bytes: if dimension == "memory" {
                6 * GIB
            } else {
                32 * GIB
            },
            cpu_cores: if dimension == "cpu" { 3 } else { 4 },
            disk_bytes: if dimension == "disk" {
                36 * GIB
            } else {
                128 * GIB
            },
        };
        store.grant("admin-device", admin.id, quota, true).unwrap();
        let a = create(&store, "admin-device", "a", 2);
        let b = create(&store, "admin-device", "b", 2);
        for op in [&a, &b] {
            store
                .complete(op.id, "running".into(), None, false)
                .unwrap();
        }
        let requests = [a.vm_id, b.vm_id]
            .into_iter()
            .map(|id| {
                let mut changes = edit_spec(&store.vm(id).unwrap());
                match dimension {
                    "memory" => changes.memory_gib = 4,
                    "cpu" => changes.cores = 2,
                    _ => changes.disk_gib = 22,
                }
                (
                    "admin-device".into(),
                    Uuid::new_v4(),
                    edit_request(id, changes, true),
                )
            })
            .collect();
        let results = race_requests(&store, requests);
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{dimension}"
        );
        let used = store.users().unwrap()[0].committed;
        match dimension {
            "memory" => assert_eq!(used.memory_bytes, quota.memory_bytes),
            "cpu" => assert_eq!(used.cpu_cores, quota.cpu_cores),
            _ => assert_eq!(used.disk_bytes, quota.disk_bytes),
        }
    }
}

fn edit_gpus(store: &Store, id: Uuid, pcis: &[&str]) -> Operation {
    let vm = store.vm(id).unwrap();
    let mut changes = edit_spec(&vm);
    changes.gpu = GpuEdit::SetMany {
        pcis: pcis.iter().map(|p| p.to_string()).collect(),
    };
    store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            edit_request(id, changes, vm.desired == Lifecycle::Running),
        )
        .unwrap()
}
#[test]
fn overlapping_multi_gpu_starts_claim_the_entire_set_or_nothing_across_connections() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 2, 2);
    let a = stopped_vm(&store, "admin-device", "a", 1);
    let b = stopped_vm(&store, "admin-device", "b", 1);
    for (id, devices) in [
        (a, ["0000:01:00.0", "0000:02:00.0"]),
        (b, ["0000:02:00.0", "0000:03:00.0"]),
    ] {
        let op = edit_gpus(&store, id, &devices);
        assert_eq!(op.phase, "succeeded");
    }
    assert_eq!(lease_count(&store, "gpu_leases"), 0);
    assert_eq!(lease_count(&store, "cpu_leases"), 0);
    let results = race_requests(
        &store,
        [a, b]
            .into_iter()
            .map(|vm_id| {
                (
                    "admin-device".into(),
                    Uuid::new_v4(),
                    Mutation::Start {
                        vm_id,
                        without_gpu: false,
                    },
                )
            })
            .collect(),
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    assert_eq!(lease_count(&store, "cpu_leases"), 2); // SMT siblings of one physical core
    let loser = [a, b][results.iter().position(Result::is_err).unwrap()];
    let vm = store.vm(loser).unwrap();
    assert!(!vm.has_compute() && vm.gpus.is_empty());
    assert_eq!(vm.preferred_gpus.len(), 2);
    assert_eq!(vm.desired, Lifecycle::Stopped);
    let fallback = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: loser,
                without_gpu: true,
            },
        )
        .unwrap();
    store
        .complete(fallback.id, "running".into(), None, false)
        .unwrap();
    let vm = store.vm(loser).unwrap();
    assert!(vm.gpus.is_empty() && vm.preferred_gpus.len() == 2);
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
}
#[test]
fn multi_gpu_replacement_holds_union_through_failure_and_releases_only_removed_devices() {
    let (dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let created = create(&store, "admin-device", "multi", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let attach = edit_gpus(&store, created.vm_id, &["0000:01:00.0", "0000:02:00.0"]);
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    let edit = edit_gpus(&store, created.vm_id, &["0000:02:00.0", "0000:03:00.0"]);
    store
        .operation_phase(
            edit.id,
            "needs_review",
            Some("injected second-card cleanup failure".into()),
        )
        .unwrap();
    let reopened = Store::open(dir.path().join("state.db"), store.limits().clone()).unwrap();
    assert_eq!(lease_count(&reopened, "gpu_leases"), 3);
    let vm = reopened.vm(created.vm_id).unwrap();
    for pci in ["0000:01:00.0", "0000:02:00.0", "0000:03:00.0"] {
        assert!(vm.claims_gpu(pci));
    }
    assert!(
        reopened
            .complete(edit.id, "running".into(), None, false)
            .is_err()
    );
    reopened.retry_operation("admin-device", edit.id).unwrap();
    reopened
        .complete(edit.id, "running".into(), None, false)
        .unwrap();
    let vm = reopened.vm(created.vm_id).unwrap();
    assert_eq!(vm.gpus, vec!["0000:02:00.0", "0000:03:00.0"]);
    assert_eq!(vm.preferred_gpus, vm.gpus);
    assert!(vm.pending_gpus.is_empty());
    assert_eq!(lease_count(&reopened, "gpu_leases"), 2);
    let stop = reopened
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: vm.id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    assert_eq!(lease_count(&reopened, "gpu_leases"), 2);
    assert!(
        reopened
            .complete(stop.id, "stopped".into(), None, false)
            .is_err()
    );
    reopened
        .complete(stop.id, "stopped".into(), None, true)
        .unwrap();
    assert_eq!(lease_count(&reopened, "gpu_leases"), 0);
    assert_eq!(reopened.vm(vm.id).unwrap().preferred_gpus.len(), 2);
}
#[test]
fn multi_gpu_edits_normalize_idempotency_and_permission_revocation_is_prospective() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let created = create(&store, "admin-device", "multi", 4);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(created.vm_id).unwrap());
    changes.gpu = GpuEdit::SetMany {
        pcis: vec![
            "0000:0B:00.0".into(),
            "0000:0A:00.0".into(),
            "0000:0b:00.0".into(),
        ],
    };
    let key = Uuid::new_v4();
    let op = store
        .submit(
            "admin-device",
            key,
            edit_request(created.vm_id, changes.clone(), true),
        )
        .unwrap();
    changes.gpu = GpuEdit::SetMany {
        pcis: vec!["0000:0a:00.0".into(), "0000:0b:00.0".into()],
    };
    assert_eq!(
        store
            .submit(
                "admin-device",
                key,
                edit_request(created.vm_id, changes.clone(), true)
            )
            .unwrap()
            .id,
        op.id
    );
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    store
        .complete(op.id, "running".into(), None, false)
        .unwrap();
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Gpu {
                    vm_id: created.vm_id,
                    pci: None
                }
            )
            .is_err()
    );
    let mut legacy = changes.clone();
    legacy.gpu = GpuEdit::Set { pci: None };
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                edit_request(created.vm_id, legacy, true)
            )
            .is_err()
    );
    store
        .grant(
            "admin-device",
            admin.id,
            store.users().unwrap()[0].grant,
            false,
        )
        .unwrap();
    changes.gpu = GpuEdit::SetMany {
        pcis: vec!["0000:0a:00.0".into(), "0000:0c:00.0".into()],
    };
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                edit_request(created.vm_id, changes, true)
            )
            .is_err()
    );
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    let remove = edit_gpus(&store, created.vm_id, &["0000:0b:00.0"]);
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    store
        .complete(remove.id, "running".into(), None, false)
        .unwrap();
    assert_eq!(store.vm(created.vm_id).unwrap().gpus, vec!["0000:0b:00.0"]);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
}
#[test]
fn overlapping_multi_gpu_edits_roll_back_every_gpu_and_resource_change_for_the_loser() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 16, 2);
    let a = create(&store, "admin-device", "a", 2);
    store.complete(a.id, "running".into(), None, false).unwrap();
    let b = create(&store, "admin-device", "b", 2);
    store.complete(b.id, "running".into(), None, false).unwrap();
    let requests = [a.vm_id, b.vm_id]
        .into_iter()
        .enumerate()
        .map(|(i, id)| {
            let mut changes = edit_spec(&store.vm(id).unwrap());
            changes.name = "changed".into();
            changes.memory_gib = 4;
            changes.disk_gib = 20;
            changes.gpu = GpuEdit::SetMany {
                pcis: if i == 0 {
                    vec!["0000:01:00.0".into(), "0000:02:00.0".into()]
                } else {
                    vec!["0000:02:00.0".into(), "0000:03:00.0".into()]
                },
            };
            (
                "admin-device".into(),
                Uuid::new_v4(),
                edit_request(id, changes, true),
            )
        })
        .collect();
    let results = race_requests(&store, requests);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    let loser = [a.vm_id, b.vm_id][results.iter().position(Result::is_err).unwrap()];
    let vm = store.vm(loser).unwrap();
    assert!(vm.gpus.is_empty() && vm.pending_gpus.is_empty() && vm.preferred_gpus.is_empty());
    assert_eq!(vm.spec.memory_gib, 2);
    assert_eq!(vm.spec.disk_gib, 10);
    assert_ne!(vm.spec.name, "changed");
    assert_eq!(store.users().unwrap()[0].committed.memory_bytes, 6 * GIB);
}

#[test]
fn gpu_attach_resize_and_multi_gpu_edits_use_the_normal_vm_memory_minimum() {
    let (_dir, store, admin) = fixture();
    grant(&store, admin.id, 2, 1);
    let created = create(&store, "admin-device", "small-gpu", 1);
    store
        .complete(created.id, "running".into(), None, false)
        .unwrap();
    let attach = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Gpu {
                vm_id: created.vm_id,
                pci: Some("0000:01:00.0".into()),
            },
        )
        .unwrap();
    store
        .complete(attach.id, "running".into(), None, false)
        .unwrap();
    let resize = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: created.vm_id,
                memory_gib: 2,
                cores: 1,
                disk_gib: 10,
            },
        )
        .unwrap();
    store
        .complete(resize.id, "running".into(), None, false)
        .unwrap();
    let resize = store
        .submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Resize {
                vm_id: created.vm_id,
                memory_gib: 1,
                cores: 1,
                disk_gib: 10,
            },
        )
        .unwrap();
    assert_eq!(
        store.vm(created.vm_id).unwrap().resources.memory_bytes,
        2 * GIB
    );
    store
        .complete(resize.id, "running".into(), None, false)
        .unwrap();
    let mut changes = edit_spec(&store.vm(created.vm_id).unwrap());
    changes.memory_gib = 0;
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                edit_request(created.vm_id, changes, true)
            )
            .is_err()
    );
    assert!(
        store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Resize {
                    vm_id: created.vm_id,
                    memory_gib: 0,
                    cores: 1,
                    disk_gib: 10
                }
            )
            .is_err()
    );
    assert_eq!(store.vm(created.vm_id).unwrap().resources.memory_bytes, GIB);
    assert_eq!(lease_count(&store, "gpu_leases"), 1);
    let add = edit_gpus(&store, created.vm_id, &["0000:01:00.0", "0000:02:00.0"]);
    assert_eq!(lease_count(&store, "gpu_leases"), 2);
    store
        .complete(add.id, "running".into(), None, false)
        .unwrap();
    let vm = store.vm(created.vm_id).unwrap();
    assert_eq!(vm.gpus.len(), 2);
    assert_eq!(vm.spec.memory_gib, 1);
    assert_eq!(vm.resources.memory_bytes, GIB);
}

#[test]
fn deleting_an_incomplete_creation_fences_late_completion_and_retains_claims_until_cleanup() {
    for phase in ["pending", "applying", "retrying", "needs_review"] {
        let (_dir, store, user) = fixture();
        grant(&store, user.id, 8, 2);
        let create = store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::Create {
                    spec: spec("cancel incomplete", 4),
                },
            )
            .unwrap();
        if phase != "pending" {
            store
                .operation_phase(
                    create.id,
                    phase,
                    Some("injected incomplete provisioning".into()),
                )
                .unwrap();
        }
        let delete = store
            .submit(
                "admin-device",
                Uuid::new_v4(),
                Mutation::State {
                    vm_id: create.vm_id,
                    desired: Lifecycle::Deleted,
                },
            )
            .unwrap();
        assert_eq!(
            store.vm(create.vm_id).unwrap().resources.memory_bytes,
            4 * GIB
        );
        assert!(!store.vm(create.vm_id).unwrap().cpu_ids.is_empty());
        assert!(store.prepare_apply(create.id).unwrap().is_none());
        assert!(
            store
                .complete(create.id, "running".into(), None, false)
                .is_err()
        );
        assert_eq!(store.vm(create.vm_id).unwrap().desired, Lifecycle::Deleted);
        store.prepare_apply(delete.id).unwrap();
        assert!(
            store
                .complete(delete.id, "deleted".into(), None, false)
                .is_err()
        );
        assert_eq!(
            store.vm(create.vm_id).unwrap().resources.memory_bytes,
            4 * GIB
        );
        store
            .complete(delete.id, "deleted".into(), None, true)
            .unwrap();
        assert_eq!(
            store.vm(create.vm_id).unwrap().resources,
            Resources::default()
        );
    }
}

#[test]
fn setup_progress_survives_apply_restart_and_never_releases_claims() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 4, 2);
    let op = create(&s, "admin-device", "setup", 4);
    let before = s.vm(op.vm_id).unwrap().resources;
    let progress = VmSetupProgress {
        stage: VmSetupStage::GuestSetup,
        since: 123,
    };
    s.observe_setup_progress(op.vm_id, progress.clone())
        .unwrap();
    s.prepare_apply(op.id).unwrap();
    assert_eq!(
        s.operations().unwrap()[0].progress.as_ref(),
        Some(&progress)
    );
    s.operation_phase(op.id, "needs_review", Some("conflict: setup failed".into()))
        .unwrap();
    assert_eq!(s.vm(op.vm_id).unwrap().resources, before);
    s.retry_operation("admin-device", op.id).unwrap();
    assert_eq!(
        s.operations().unwrap()[0].progress.as_ref(),
        Some(&progress)
    );
    s.complete(op.id, "running".into(), None, false).unwrap();
    s.observe_setup_progress(
        op.vm_id,
        VmSetupProgress {
            stage: VmSetupStage::GuestAgent,
            since: 456,
        },
    )
    .unwrap();
    assert_eq!(
        s.operations().unwrap()[0].progress.as_ref(),
        Some(&progress)
    );
}

#[test]
fn unfinished_setup_cannot_disappear_behind_recent_operation_history() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 4, 2);
    let stuck = create(&s, "admin-device", "needs repair", 1);
    s.operation_phase(
        stuck.id,
        "needs_review",
        Some("conflict: setup failed".into()),
    )
    .unwrap();
    let other = create(&s, "admin-device", "other", 1);
    s.complete(other.id, "running".into(), None, false).unwrap();
    for i in 0..205 {
        s.submit(
            "admin-device",
            Uuid::new_v4(),
            Mutation::Edit {
                vm_id: other.vm_id,
                changes: WorkspaceEdit {
                    name: format!("name {i}"),
                    memory_gib: 1,
                    cores: 1,
                    disk_gib: 10,
                    gpu: GpuEdit::Keep,
                },
                restart_confirmed: false,
            },
        )
        .unwrap();
    }
    let operations = s.operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|op| op.id == stuck.id && op.phase == "needs_review")
    );
    assert_eq!(
        operations
            .iter()
            .filter(|op| op.phase == "succeeded")
            .count(),
        200
    );
}
