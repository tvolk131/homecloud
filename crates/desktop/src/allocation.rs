//! Presentation-only accounting. These values describe durable backing, never
//! observed utilization, and never participate in server-side admission.
use homecloud_core::{GIB, Resources, Role, Snapshot, User};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceKind {
    #[default]
    Memory,
    Cpu,
    Storage,
}
impl ResourceKind {
    pub const ALL: [Self; 3] = [Self::Memory, Self::Cpu, Self::Storage];
    pub fn label(self) -> &'static str {
        match self {
            Self::Memory => "Memory",
            Self::Cpu => "CPU",
            Self::Storage => "Storage",
        }
    }
    pub fn value(self, resources: Resources) -> u128 {
        match self {
            Self::Memory => resources.memory_bytes as u128,
            Self::Cpu => resources.cpu_cores as u128,
            Self::Storage => resources.disk_bytes as u128,
        }
    }
    pub fn format(self, value: u128) -> String {
        if self == Self::Cpu {
            format!("{value} {}", if value == 1 { "core" } else { "cores" })
        } else {
            let (divisor, unit) = if value >= 1024 * GIB as u128 {
                (1024 * GIB as u128, "TiB")
            } else if value != 0 && value < GIB as u128 {
                (GIB as u128 / 1024, "MiB")
            } else {
                (GIB as u128, "GiB")
            };
            if value.is_multiple_of(divisor) {
                format!("{} {unit}", value / divisor)
            } else {
                format!("{:.1} {unit}", value as f64 / divisor as f64)
            }
        }
    }
}

pub struct OwnerShare<'a> {
    pub user: &'a User,
    pub grant: u128,
    pub held: u128,
}
impl OwnerShare<'_> {
    pub fn backed(&self) -> u128 {
        self.grant.max(self.held)
    }
    pub fn unused(&self) -> u128 {
        self.grant.saturating_sub(self.held)
    }
}
pub struct Allocation<'a> {
    pub kind: ResourceKind,
    pub capacity: u128,
    pub owners: Vec<OwnerShare<'a>>,
}
impl<'a> Allocation<'a> {
    pub fn new(snapshot: &'a Snapshot, kind: ResourceKind) -> Self {
        let admin = snapshot.me.role == Role::Admin;
        let mut users: Vec<_> = if admin {
            snapshot.users.iter().collect()
        } else {
            vec![&snapshot.me]
        };
        users.sort_by_key(|user| (user.name.to_lowercase(), user.id));
        Self {
            kind,
            capacity: kind.value(if admin {
                snapshot.capacity
            } else {
                snapshot.me.grant
            }),
            owners: users
                .into_iter()
                .map(|user| OwnerShare {
                    user,
                    grant: kind.value(user.grant),
                    held: kind.value(user.committed),
                })
                .collect(),
        }
    }
    pub fn held(&self) -> u128 {
        self.owners.iter().map(|o| o.held).sum()
    }
    pub fn backed(&self) -> u128 {
        // A lowered grant does not release existing commitments, even if that
        // user is disabled. Match the store's component-wise max rule.
        self.owners.iter().map(OwnerShare::backed).sum()
    }
    pub fn unissued(&self) -> u128 {
        self.capacity.saturating_sub(self.backed())
    }
    pub fn scale(&self) -> u128 {
        self.capacity.max(self.backed())
    }
    pub fn selected(&self, id: Option<Uuid>) -> Option<&OwnerShare<'a>> {
        id.and_then(|id| self.owners.iter().find(|o| o.user.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_tests::{fixture, id};

    #[test]
    fn reduced_grants_and_disabled_users_keep_their_backing() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.capacity.memory_bytes = 24 * GIB;
        s.users[0].grant.memory_bytes = 2 * GIB;
        s.users[0].committed.memory_bytes = 8 * GIB;
        s.users[1].active = false;
        let a = Allocation::new(s, ResourceKind::Memory);
        assert_eq!(a.held(), 8 * GIB as u128);
        assert_eq!(a.backed(), 16 * GIB as u128);
        assert_eq!(a.unissued(), 8 * GIB as u128);
        let tommy = a.selected(Some(id(1))).unwrap();
        assert_eq!(tommy.unused(), 0);
        assert_eq!(tommy.backed(), 8 * GIB as u128);
    }

    #[test]
    fn member_charts_never_include_other_people_or_host_unissued_capacity() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.me.role = Role::User;
        let a = Allocation::new(s, ResourceKind::Memory);
        assert_eq!(a.owners.len(), 1);
        assert_eq!(a.owners[0].user.id, s.me.id);
        assert_eq!(a.capacity, s.me.grant.memory_bytes as u128);
        assert_eq!(a.unissued(), 0);
        assert_eq!(a.owners[0].unused(), 12 * GIB as u128);
    }

    #[test]
    fn pending_and_stopped_claims_come_from_the_ledger_not_observed_state() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.vms[0].observed = "stopped".into();
        let before = Allocation::new(s, ResourceKind::Memory).held();
        assert_eq!(before, 4 * GIB as u128);
        s.users[0].committed.memory_bytes = 0;
        assert_eq!(Allocation::new(s, ResourceKind::Memory).held(), 0);
        // Stopping releases compute only; disk backing is independent of ZFS free space.
        s.inventory.pool_available_bytes = 0;
        assert_eq!(
            Allocation::new(s, ResourceKind::Storage).held(),
            84 * GIB as u128
        );
    }

    #[test]
    fn zero_and_inconsistent_capacity_do_not_erase_allocations_or_overflow() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.capacity.cpu_cores = 0;
        s.users[0].committed.cpu_cores = u64::MAX;
        s.users[1].committed.cpu_cores = u64::MAX;
        let a = Allocation::new(s, ResourceKind::Cpu);
        assert_eq!(a.unissued(), 0);
        assert_eq!(a.scale(), 2 * u64::MAX as u128);
        assert_eq!(ResourceKind::Storage.format(1536 * GIB as u128), "1.5 TiB");
    }
}
