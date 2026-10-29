use super::*;
fn fixture() -> (tempfile::TempDir, Store, User) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        dir.path().join("state.db"),
        Limits {
            capacity: Resources {
                memory_bytes: 8 * GIB,
                cpu_cores: 4,
                disk_bytes: 1000 * GIB,
            },
            core_groups: vec![vec![2, 8], vec![3, 9], vec![4, 10], vec![5, 11]],
            disk_overhead_gib_per_vm: 2,
        },
    )
    .unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    let user = store.enroll(&token, "admin").unwrap();
    (dir, store, user)
}
fn policy(store: &Store, op: &HostReserveOperation) -> HostReservePolicy {
    HostReservePolicy {
        excluded_network_slots: vec![],
        memory: None,
        envelope: store.limits.capacity,
        core_groups: store.limits.core_groups.clone(),
        disk_overhead_gib_per_vm: 2,
        minimum_reserves: Resources::default(),
        arc: None,
        memory_policy: None,
        base_pool_free_bytes: 30 * GIB,
        state: op.target.clone(),
        applying: false,
        operation_id: Some(op.id),
    }
}
#[test]
fn unused_promises_and_existing_commitments_both_limit_system_reserves() {
    let (_dir, s, admin) = fixture();
    s.grant(
        "admin",
        admin.id,
        Resources {
            memory_bytes: 4 * GIB,
            cpu_cores: 2,
            disk_bytes: 900 * GIB,
        },
        false,
    )
    .unwrap();
    let key = Uuid::new_v4();
    assert!(
        s.set_host_reserves(
            "admin",
            key,
            0,
            Resources {
                disk_bytes: 101 * GIB,
                ..Default::default()
            }
        )
        .is_err()
    );
    let op = s
        .set_host_reserves(
            "admin",
            key,
            0,
            Resources {
                disk_bytes: 100 * GIB,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(s.capacity().unwrap().disk_bytes, 900 * GIB);
    assert_eq!(
        s.set_host_reserves("admin", key, 0, op.target.additional)
            .unwrap()
            .id,
        op.id
    );
    s.finish_host_reserves(op.id, &policy(&s, &op)).unwrap();
    let reopened = Store::open(&*s.path, s.limits.clone()).unwrap();
    assert_eq!(reopened.capacity().unwrap().disk_bytes, 900 * GIB);
    let release = s
        .set_host_reserves("admin", Uuid::new_v4(), 1, Resources::default())
        .unwrap();
    assert_eq!(
        s.capacity().unwrap().disk_bytes,
        900 * GIB,
        "Unconfirmed host release cannot be reissued"
    );
    s.finish_host_reserves(release.id, &policy(&s, &release))
        .unwrap();
    assert_eq!(s.capacity().unwrap().disk_bytes, 1000 * GIB);
}

fn vm_spec(name: &str, disk: u64) -> VmSpec {
    VmSpec {
        applications: vec![],
        name: name.into(),
        memory_gib: 1,
        cores: 1,
        disk_gib: disk,
        ssh_public_key:
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into(),
    }
}
fn baseline_policy(s: &Store) -> HostReservePolicy {
    HostReservePolicy {
        excluded_network_slots: vec![],
        memory: None,
        envelope: s.limits.capacity,
        core_groups: s.limits.core_groups.clone(),
        disk_overhead_gib_per_vm: 2,
        minimum_reserves: Resources::default(),
        arc: None,
        memory_policy: None,
        base_pool_free_bytes: 30 * GIB,
        state: s.host_reserve_state().unwrap(),
        applying: false,
        operation_id: None,
    }
}
#[test]
fn disabled_users_and_above_quota_vm_backing_cannot_be_reclaimed_as_reserves() {
    let (_dir, s, _admin) = fixture();
    let (token, _, _) = s
        .invite(
            "admin",
            "Coworker",
            Role::User,
            Resources {
                memory_bytes: GIB,
                cpu_cores: 1,
                disk_bytes: 900 * GIB,
            },
            false,
            24,
        )
        .unwrap();
    let owner = s.enroll(&token, "owner").unwrap();
    s.submit(
        "owner",
        Uuid::new_v4(),
        Mutation::Create {
            spec: vm_spec("held", 848),
        },
    )
    .unwrap();
    s.set_account_active("admin", owner.id, false).unwrap();
    s.grant(
        "admin",
        owner.id,
        Resources {
            disk_bytes: 800 * GIB,
            ..Default::default()
        },
        false,
    )
    .unwrap();
    assert!(
        s.set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                disk_bytes: 151 * GIB,
                ..Default::default()
            }
        )
        .is_err()
    );
    let op = s
        .set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                disk_bytes: 150 * GIB,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(s.capacity().unwrap().disk_bytes, 850 * GIB);
    assert_eq!(
        s.users()
            .unwrap()
            .iter()
            .find(|u| u.id == owner.id)
            .unwrap()
            .committed
            .disk_bytes,
        850 * GIB
    );
    s.finish_host_reserves(op.id, &policy(&s, &op)).unwrap();
}
#[test]
fn stopping_compute_is_held_until_cleanup_and_stopped_disks_stay_protected() {
    let (_dir, s, admin) = fixture();
    s.grant(
        "admin",
        admin.id,
        Resources {
            memory_bytes: GIB,
            cpu_cores: 1,
            disk_bytes: 12 * GIB,
        },
        false,
    )
    .unwrap();
    let create = s
        .submit(
            "admin",
            Uuid::new_v4(),
            Mutation::Create {
                spec: vm_spec("devbox", 10),
            },
        )
        .unwrap();
    s.complete(create.id, "running".into(), None, false)
        .unwrap();
    s.grant("admin", admin.id, Resources::default(), false)
        .unwrap();
    let stop = s
        .submit(
            "admin",
            Uuid::new_v4(),
            Mutation::State {
                vm_id: create.vm_id,
                desired: Lifecycle::Stopped,
            },
        )
        .unwrap();
    let all_compute = Resources {
        memory_bytes: 8 * GIB,
        cpu_cores: 4,
        disk_bytes: 0,
    };
    assert!(
        s.set_host_reserves("admin", Uuid::new_v4(), 0, all_compute)
            .is_err()
    );
    s.complete(stop.id, "stopped".into(), None, true).unwrap();
    assert!(
        s.set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                disk_bytes: 1000 * GIB,
                ..Default::default()
            }
        )
        .is_err()
    );
    let op = s
        .set_host_reserves("admin", Uuid::new_v4(), 0, all_compute)
        .unwrap();
    assert_eq!(op.target.core_groups.len(), 4);
    assert_eq!(s.vm(create.vm_id).unwrap().resources.disk_bytes, 12 * GIB);
}
#[test]
fn a_concurrent_grant_and_reserve_cannot_both_spend_the_same_capacity() {
    use std::sync::{Arc, Barrier};
    let (_dir, s, _) = fixture();
    let (_, user, _) = s
        .invite(
            "admin",
            "Coworker",
            Role::User,
            Resources::default(),
            false,
            24,
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let a = Store::open(&*s.path, s.limits.clone()).unwrap();
    let b = a.clone();
    let gate = barrier.clone();
    let one = std::thread::spawn(move || {
        gate.wait();
        a.grant(
            "admin",
            user,
            Resources {
                cpu_cores: 2,
                ..Default::default()
            },
            false,
        )
        .is_ok()
    });
    let gate = barrier.clone();
    let two = std::thread::spawn(move || {
        gate.wait();
        b.set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                cpu_cores: 3,
                ..Default::default()
            },
        )
        .is_ok()
    });
    barrier.wait();
    assert_eq!(
        [one.join().unwrap(), two.join().unwrap()]
            .into_iter()
            .filter(|v| *v)
            .count(),
        1
    );
    let protected = s
        .users()
        .unwrap()
        .iter()
        .map(|u| u.grant.cpu_cores.max(u.committed.cpu_cores))
        .sum::<u64>();
    assert!(protected <= s.capacity().unwrap().cpu_cores);
}
#[test]
fn concurrent_creates_and_reservation_claim_different_complete_cores() {
    use std::sync::{Arc, Barrier};
    let (_dir, s, admin) = fixture();
    s.grant(
        "admin",
        admin.id,
        Resources {
            memory_bytes: 2 * GIB,
            cpu_cores: 2,
            disk_bytes: 24 * GIB,
        },
        false,
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = vec![];
    for i in 0..2 {
        let other = Store::open(&*s.path, s.limits.clone()).unwrap();
        let gate = barrier.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            other
                .submit(
                    "admin",
                    Uuid::new_v4(),
                    Mutation::Create {
                        spec: vm_spec(&format!("vm-{i}"), 10),
                    },
                )
                .unwrap();
        }));
    }
    let other = Store::open(&*s.path, s.limits.clone()).unwrap();
    let gate = barrier.clone();
    let reserve = std::thread::spawn(move || {
        gate.wait();
        other
            .set_host_reserves(
                "admin",
                Uuid::new_v4(),
                0,
                Resources {
                    cpu_cores: 2,
                    ..Default::default()
                },
            )
            .unwrap()
    });
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    let op = reserve.join().unwrap();
    let held = s
        .vms()
        .unwrap()
        .into_iter()
        .flat_map(|v| v.cpu_ids)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(held.len(), 4);
    assert!(
        op.target
            .core_groups
            .iter()
            .flatten()
            .all(|cpu| !held.contains(cpu))
    );
    assert_eq!(s.capacity().unwrap().cpu_cores, 2);
}
#[test]
fn failed_release_survives_reopen_and_requires_confirmed_enforcement() {
    let (_dir, s, _) = fixture();
    let held = Resources {
        memory_bytes: 2 * GIB,
        cpu_cores: 2,
        disk_bytes: 100 * GIB,
    };
    let op = s
        .set_host_reserves("admin", Uuid::new_v4(), 0, held)
        .unwrap();
    let mut applying = policy(&s, &op);
    applying.applying = true;
    assert!(s.finish_host_reserves(op.id, &applying).is_err());
    s.finish_host_reserves(op.id, &policy(&s, &op)).unwrap();
    let release = s
        .set_host_reserves("admin", Uuid::new_v4(), 1, Resources::default())
        .unwrap();
    s.prepare_host_reserves().unwrap();
    s.fail_host_reserves(release.id, "Injected interruption".into())
        .unwrap();
    let reopened = Store::open(&*s.path, s.limits.clone()).unwrap();
    assert_eq!(
        reopened.capacity().unwrap(),
        s.limits.capacity.checked_sub(held).unwrap()
    );
    assert_eq!(
        reopened.host_reserve_operation().unwrap().unwrap().phase,
        ReservePhase::NeedsReview
    );
    reopened.retry_host_reserves("admin", release.id).unwrap();
    reopened
        .finish_host_reserves(release.id, &policy(&s, &release))
        .unwrap();
    assert_eq!(reopened.capacity().unwrap(), s.limits.capacity);
    let mut unexpected = policy(&s, &release);
    unexpected.state.revision = 99;
    assert!(reopened.verify_host_reserve_policy(&unexpected).is_err());
}
#[test]
fn only_admins_can_change_reserves_and_only_unapplied_failures_can_be_discarded() {
    let (_dir, s, _) = fixture();
    let (token, _, _) = s
        .invite("admin", "User", Role::User, Resources::default(), false, 24)
        .unwrap();
    s.enroll(&token, "user").unwrap();
    assert!(
        s.set_host_reserves("user", Uuid::new_v4(), 0, Resources::default())
            .is_err()
    );
    let op = s
        .set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                cpu_cores: 1,
                ..Default::default()
            },
        )
        .unwrap();
    s.fail_host_reserves(op.id, "Before effects".into())
        .unwrap();
    assert!(s.retry_host_reserves("user", op.id).is_err());
    assert!(
        s.discard_host_reserves("user", op.id, &baseline_policy(&s))
            .is_err()
    );
    assert!(
        s.discard_host_reserves("admin", op.id, &policy(&s, &op))
            .is_err()
    );
    s.prepare_reserve_discard("admin", op.id).unwrap();
    s.discard_host_reserves("admin", op.id, &baseline_policy(&s))
        .unwrap();
    assert_eq!(s.capacity().unwrap(), s.limits.capacity);
    assert_eq!(
        s.host_reserve_view(&baseline_policy(&s))
            .unwrap()
            .operation
            .unwrap()
            .phase,
        ReservePhase::Cancelled
    );
    assert!(
        s.set_host_reserves("admin", Uuid::new_v4(), 99, Resources::default())
            .is_err()
    );
}

#[test]
fn arc_ceiling_is_elastic_and_idempotency_includes_its_mode() {
    let (_dir, s, _admin) = fixture();
    let key = Uuid::new_v4();
    let before = s.capacity().unwrap();
    let mode = Some(ArcMode::Custom { max_bytes: 2 * GIB });
    let op = s
        .set_host_policy(
            "admin",
            key,
            0,
            Resources::default(),
            mode.clone(),
            Some(memory_policy()),
        )
        .unwrap();
    assert_eq!(
        s.capacity().unwrap(),
        before,
        "Cache ceiling cannot consume RAM promises"
    );
    assert_eq!(
        s.set_host_policy(
            "admin",
            key,
            0,
            Resources::default(),
            mode,
            Some(memory_policy())
        )
        .unwrap()
        .id,
        op.id
    );
    assert!(
        s.set_host_policy(
            "admin",
            key,
            0,
            Resources::default(),
            Some(ArcMode::Automatic),
            Some(memory_policy()),
        )
        .is_err()
    );
    s.finish_host_reserves(op.id, &policy(&s, &op)).unwrap();
    let automatic = s
        .set_host_policy(
            "admin",
            Uuid::new_v4(),
            1,
            Resources::default(),
            Some(ArcMode::Automatic),
            Some(memory_policy()),
        )
        .unwrap();
    assert_eq!(s.capacity().unwrap(), before);
    assert_eq!(automatic.target.arc, Some(ArcMode::Automatic));
    assert!(
        s.set_host_policy(
            "admin",
            Uuid::new_v4(),
            1,
            Resources::default(),
            Some(ArcMode::Custom { max_bytes: 0 }),
            Some(memory_policy()),
        )
        .is_err()
    );
}

fn memory_policy() -> HostMemoryPolicy {
    HostMemoryPolicy {
        operating_floor_bytes: 2 * GIB,
        recommended_arc_bytes: 2 * GIB,
        minimum_system_bytes: 4 * GIB,
    }
}

#[test]
fn arc_growth_requires_system_backing_and_rejected_requests_hold_no_capacity() {
    let (_dir, s, _) = fixture();
    let before = s.capacity().unwrap();
    assert!(
        s.set_host_policy(
            "admin",
            Uuid::new_v4(),
            0,
            Resources::default(),
            Some(ArcMode::Custom { max_bytes: 3 * GIB }),
            Some(memory_policy())
        )
        .is_err()
    );
    assert_eq!(s.capacity().unwrap(), before);
    assert!(s.host_reserve_operation().unwrap().is_none());
    let extra = Resources {
        memory_bytes: GIB,
        ..Resources::default()
    };
    let op = s
        .set_host_policy(
            "admin",
            Uuid::new_v4(),
            0,
            extra,
            Some(ArcMode::Custom { max_bytes: 3 * GIB }),
            Some(memory_policy()),
        )
        .unwrap();
    assert_eq!(
        s.capacity().unwrap().memory_bytes,
        before.memory_bytes - GIB
    );
    assert_eq!(op.target.additional, extra);
}

#[test]
fn nested_uplink_subnet_is_never_allocated_as_a_guest_bridge() {
    let (_dir, s, u) = fixture();
    let mut p = baseline_policy(&s);
    p.excluded_network_slots = vec![1, 3];
    s.verify_host_reserve_policy(&p).unwrap();
    s.grant("admin", u.id, s.limits.capacity, false).unwrap();
    let op=s.submit("admin",Uuid::new_v4(),Mutation::Create { spec:VmSpec { applications:vec![],name:"Subnet proof".into(),memory_gib:1,cores:1,disk_gib:10,ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into() } }).unwrap();
    assert_eq!(s.vm(op.vm_id).unwrap().network_slot, 2);
    p.excluded_network_slots = vec![2];
    assert!(s.verify_host_reserve_policy(&p).is_err());
}

#[test]
fn applied_reserve_receipt_cannot_release_its_fence_until_arc_is_enforced() {
    let (_dir, s, _) = fixture();
    let op = s
        .set_host_reserves(
            "admin",
            Uuid::new_v4(),
            0,
            Resources {
                memory_bytes: GIB,
                ..Resources::default()
            },
        )
        .unwrap();
    let mut p = policy(&s, &op);
    p.memory = Some(MemoryCapacityStatus {
        revision: 0,
        reviewed_bytes: 8 * GIB,
        safe_bytes: 8 * GIB,
        physical_bytes: 12 * GIB,
        minimum_system_bytes: 4 * GIB,
        boot_id: Uuid::nil().to_string(),
        observed_at: now(),
        arc_ready: false,
        error: None,
    });
    assert!(s.finish_host_reserves(op.id, &p).is_err());
    assert_eq!(s.host_reserve_state().unwrap(), op.before);
    assert!(s.host_reserve_operation().unwrap().is_some());
    p.memory.as_mut().unwrap().arc_ready = true;
    s.finish_host_reserves(op.id, &p).unwrap();
    assert_eq!(s.host_reserve_state().unwrap(), op.target);
}
