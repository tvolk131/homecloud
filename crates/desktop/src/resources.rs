//! Form ranges describe a snapshot of admission; the server still decides on Save.
use crate::{App, Field, Panel};
use homecloud_core::{GIB, Lifecycle, Snapshot, User};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Create,
    Edit(Uuid),
    Grant(Option<Uuid>),
}
#[derive(Debug, Clone, Copy)]
pub struct Bound {
    pub min: u64,
    pub max: u64,
}
impl Bound {
    pub fn accepts(self, value: u64) -> bool {
        (self.min..=self.max).contains(&value)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct FormBounds {
    form: Form,
    pub memory: Bound,
    pub cores: Bound,
    pub disk: Bound,
}
fn owner(s: &Snapshot, id: Uuid) -> Option<&User> {
    if s.me.id == id {
        Some(&s.me)
    } else {
        s.users.iter().find(|u| u.id == id)
    }
}
fn amount(resource: homecloud_core::Resources, field: Field) -> u64 {
    match field {
        Field::Ram | Field::RamGrant => resource.memory_bytes,
        Field::Cores | Field::CoreGrant => resource.cpu_cores,
        _ => resource.disk_bytes,
    }
}
impl FormBounds {
    fn context(app: &App) -> Option<Form> {
        app.panel
            .as_ref()
            .and_then(|panel| Self::panel_context(app, panel))
    }
    fn panel_context(app: &App, panel: &Panel) -> Option<Form> {
        match panel {
            Panel::Create => Some(Form::Create),
            Panel::EditWorkspace(id) => Some(Form::Edit(*id)),
            Panel::Grant => Some(Form::Grant(app.selected)),
            Panel::Invite => Some(Form::Grant(None)),
            _ => None,
        }
    }
    pub fn capture(app: &App) -> Option<Self> {
        let form = Self::context(app)?;
        Self::capture_form(app, form)
    }
    fn capture_form(app: &App, form: Form) -> Option<Self> {
        let s = app.snapshot.as_ref()?;
        let field_bound = |field: Field, minimum: u64, protocol_max: u64| {
            let unit = if matches!(field, Field::Cores | Field::CoreGrant) {
                1
            } else {
                GIB
            };
            let mut current = 0;
            let maximum = match form {
                Form::Create => {
                    amount(s.me.grant, field).saturating_sub(amount(s.me.committed, field))
                }
                Form::Edit(id) => {
                    let vm = s.vms.iter().find(|v| v.id == id)?;
                    let u = owner(s, vm.owner)?;
                    current = match field {
                        Field::Ram => vm.spec.memory_gib,
                        Field::Cores => vm.spec.cores,
                        _ => vm.spec.disk_gib,
                    };
                    if vm.desired == Lifecycle::Stopped && !matches!(field, Field::Disk) {
                        amount(u.grant, field).min(amount(s.capacity, field))
                    } else {
                        amount(u.grant, field).saturating_sub(
                            amount(u.committed, field).saturating_sub(amount(vm.resources, field)),
                        )
                    }
                }
                Form::Grant(id) => {
                    let others = s
                        .users
                        .iter()
                        .filter(|u| Some(u.id) != id)
                        .map(|u| amount(u.grant, field).max(amount(u.committed, field)) as u128)
                        .sum::<u128>();
                    if let Some(u) = id.and_then(|id| owner(s, id)) {
                        current = amount(u.grant, field) / unit;
                    }
                    (amount(s.capacity, field) as u128)
                        .saturating_sub(others)
                        .min(u64::MAX as u128) as u64
                }
            };
            let maximum = if matches!(field, Field::Disk) {
                maximum.saturating_sub(
                    (s.disk_overhead_gib_per_vm
                        + match form {
                            Form::Create if app.install_managed_host => {
                                homecloud_core::MANAGED_HOST_DISK_GIB
                            }
                            Form::Edit(id) => s
                                .vms
                                .iter()
                                .find(|v| v.id == id)
                                .map(|v| v.spec.application_disk_gib())
                                .unwrap_or(0),
                            _ => 0,
                        })
                    .saturating_mul(GIB),
                )
            } else {
                maximum
            };
            let min = if matches!(form, Form::Edit(_)) && matches!(field, Field::Disk) {
                current
            } else {
                minimum
            };
            Some(Bound {
                min,
                max: (maximum / unit).min(protocol_max).max(current),
            })
        };
        let grant = matches!(form, Form::Grant(_));
        let managed_host = match form {
            Form::Create => app.install_managed_host,
            Form::Edit(id) => s.vms.iter().any(|v| v.id == id && v.spec.managed_host()),
            _ => false,
        };
        Some(Self {
            form,
            memory: field_bound(
                if grant { Field::RamGrant } else { Field::Ram },
                if grant {
                    0
                } else if managed_host {
                    8
                } else {
                    1
                },
                if grant { u64::MAX / GIB } else { 1024 },
            )?,
            cores: field_bound(
                if grant {
                    Field::CoreGrant
                } else {
                    Field::Cores
                },
                if grant {
                    0
                } else if managed_host {
                    2
                } else {
                    1
                },
                if grant { u64::MAX } else { 256 },
            )?,
            disk: field_bound(
                if grant { Field::DiskGrant } else { Field::Disk },
                if grant {
                    0
                } else if managed_host {
                    32
                } else {
                    10
                },
                if grant { u64::MAX / GIB } else { 65536 },
            )?,
        })
    }
    pub fn current(app: &App) -> Option<Self> {
        app.resource_bounds
            .filter(|b| Some(b.form) == Self::context(app))
            .or_else(|| Self::capture(app))
    }
    /// Rendering retains the last form while it exits. Submission and slider
    /// events continue to use current(), which requires an active panel.
    pub fn displayed(app: &App) -> Option<Self> {
        let panel = app.panel.as_ref().or(app.last_panel.as_ref())?;
        let form = Self::panel_context(app, panel)?;
        app.resource_bounds
            .filter(|b| b.form == form)
            .or_else(|| Self::capture_form(app, form))
    }
    pub fn is_grant(self) -> bool {
        matches!(self.form, Form::Grant(_))
    }
    pub fn for_field(self, field: Field) -> Bound {
        match field {
            Field::Ram | Field::RamGrant => self.memory,
            Field::Cores | Field::CoreGrant => self.cores,
            _ => self.disk,
        }
    }
    pub fn validate(self, app: &App) -> Result<(), String> {
        let grant = matches!(self.form, Form::Grant(_));
        for (field, label) in if grant {
            [
                (Field::RamGrant, "Memory grant"),
                (Field::CoreGrant, "Core grant"),
                (Field::DiskGrant, "Disk grant"),
            ]
        } else {
            [
                (Field::Ram, "Memory"),
                (Field::Cores, "Cores"),
                (Field::Disk, "Disk"),
            ]
        } {
            let value = app
                .get(field)
                .parse::<u64>()
                .map_err(|_| "Enter whole numbers for memory, cores and disk.".to_owned())?;
            let bound = self.for_field(field);
            if !bound.accepts(value) {
                return Err(if bound.max < bound.min {
                    format!("No {label} capacity available.")
                } else if !grant
                    && matches!(self.form, Form::Edit(_))
                    && matches!(field, Field::Disk)
                    && value < bound.min
                {
                    format!("Disk can only grow. Current size: {} GiB.", bound.min)
                } else {
                    format!(
                        "{label}: choose {}–{}{}.",
                        bound.min,
                        bound.max,
                        if matches!(field, Field::Cores | Field::CoreGrant) {
                            " cores"
                        } else {
                            " GiB"
                        }
                    )
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Msg,
        visual_tests::{fixture, id},
    };

    #[test]
    fn create_bounds_use_uncommitted_quota_and_the_reported_disk_overhead() {
        let mut app = fixture();
        app.snapshot.as_mut().unwrap().disk_overhead_gib_per_vm = 5;
        let _ = app.update(Msg::Open(Panel::Create));
        let b = FormBounds::current(&app).unwrap();
        assert_eq!((b.memory.min, b.memory.max), (1, 12));
        assert_eq!((b.cores.min, b.cores.max), (1, 3));
        assert_eq!((b.disk.min, b.disk.max), (10, 151));
    }
    #[test]
    fn editing_reuses_existing_backing_and_stopped_compute_can_be_configured_for_a_later_start() {
        let mut app = fixture();
        let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
        let b = FormBounds::current(&app).unwrap();
        assert_eq!(b.memory.max, 16);
        assert_eq!(b.cores.max, 4);
        assert_eq!((b.disk.min, b.disk.max), (40, 196));
        let _ = app.update(Msg::Open(Panel::EditWorkspace(id(11))));
        assert_eq!(FormBounds::current(&app).unwrap().memory.max, 16);
    }
    #[test]
    fn reduced_quotas_preserve_current_values_and_allow_reductions_without_disk_shrink() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.me.grant.memory_bytes = 2 * GIB;
        s.me.grant.disk_bytes = 40 * GIB;
        let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
        let b = FormBounds::current(&app).unwrap();
        assert_eq!(b.memory.max, 4);
        assert_eq!((b.disk.min, b.disk.max), (40, 40));
        assert_eq!(app.get(Field::Ram), "4");
        assert!(b.validate(&app).is_ok());
        let _ = app.update(Msg::ResourceValue(Field::Ram, 2));
        assert_eq!(app.get(Field::Ram), "2");
        let _ = app.update(Msg::ResourceValue(Field::Disk, 39));
        assert_eq!(app.get(Field::Disk), "40");
        let _ = app.update(Msg::Edit(Field::Disk, "39".into()));
        assert!(b.validate(&app).unwrap_err().contains("Disk can only grow"));
        let _ = app.update(Msg::SaveWorkspace(id(10)));
        assert!(app.vm_request.is_none());
        assert!(matches!(app.panel, Some(Panel::EditWorkspace(_))));
    }
    #[test]
    fn an_admin_edit_uses_the_owners_quota() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.vms[0].owner = id(2);
        s.users[1].committed = s.vms[0].resources;
        let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
        assert_eq!(FormBounds::current(&app).unwrap().memory.max, 8);
    }
    #[test]
    fn grants_keep_other_peoples_commitments_backed_and_allow_zero() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.capacity.memory_bytes = 32 * GIB;
        s.users[0].grant.memory_bytes = 2 * GIB;
        s.users[0].committed.memory_bytes = 10 * GIB;
        s.users[0].active = false;
        let _ = app.update(Msg::Select(id(2)));
        let b = FormBounds::current(&app).unwrap();
        assert_eq!((b.memory.min, b.memory.max), (0, 22));
        let _ = app.update(Msg::ResourceValue(Field::RamGrant, 0));
        assert_eq!(app.grant().unwrap().memory_bytes, 0);
        let _ = app.update(Msg::Open(Panel::Invite));
        assert_eq!(FormBounds::current(&app).unwrap().memory.max, 14);
    }
    #[test]
    fn refresh_does_not_move_the_ranges_or_clamp_typed_values_and_busy_sliders_do_nothing() {
        let mut app = fixture();
        let _ = app.update(Msg::Open(Panel::Create));
        let _ = app.update(Msg::Edit(Field::Ram, "7".into()));
        app.snapshot.as_mut().unwrap().me.grant.memory_bytes = GIB;
        assert_eq!(FormBounds::current(&app).unwrap().memory.max, 12);
        assert_eq!(app.get(Field::Ram), "7");
        app.busy = true;
        let _ = app.update(Msg::ResourceValue(Field::Ram, 6));
        assert_eq!(app.get(Field::Ram), "7");
        app.busy = false;
        let _ = app.update(Msg::Open(Panel::Create));
        assert_eq!(app.get(Field::Ram), "1");
        assert!(FormBounds::current(&app).unwrap().validate(&app).is_err());
    }
}
