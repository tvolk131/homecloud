use super::*;
use std::sync::{Arc, Barrier};

fn fixture() -> (tempfile::TempDir, Store, User) {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        capacity: Resources {
            memory_bytes: 32 * GIB,
            cpu_cores: 2,
            disk_bytes: 128 * GIB,
        },
        core_groups: vec![vec![2, 8], vec![3, 9]],
        disk_overhead_gib_per_vm: 2,
    };
    let store = Store::open(dir.path().join("state.db"), limits).unwrap();
    let (user, token) = store.bootstrap("Operator").unwrap();
    store.enroll(&token, "admin-device").unwrap();
    (dir, store, user)
}
fn spec(name: &str, memory: u64) -> VmSpec {
    VmSpec {
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
    s.complete(op.id, "running".into(), None).unwrap();
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
    s.complete(stop.id, "stopped".into(), None).unwrap();
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 24 * GIB);
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
    assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 24 * GIB);
    s.complete(del.id, "deleted".into(), None).unwrap();
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
    restored.complete(op.id, "running".into(), None).unwrap();
    assert!(restored.pending().unwrap().is_empty());
}

#[test]
fn gpu_lease_is_exclusive_and_permission_revocation_does_not_interrupt_it() {
    let (_dir, s, admin) = fixture();
    grant(&s, admin.id, 8, 2);
    let a = create(&s, "admin-device", "a", 4);
    s.complete(a.id, "running".into(), None).unwrap();
    let b = create(&s, "admin-device", "b", 4);
    s.complete(b.id, "running".into(), None).unwrap();
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
    s.complete(attach.id, "running".into(), None).unwrap();
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
    s.complete(stop.id, "stopped".into(), None).unwrap();
    assert!(s.vm(a.vm_id).unwrap().gpu.is_some());
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
    s.complete(detach.id, "stopped".into(), None).unwrap();
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
        s.grant("coworker", admin.id, Resources::default(), false)
            .is_err()
    );
}
