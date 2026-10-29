use super::*;
use crate::resources::Bound;

pub const FIELDS: [Field; 3] = [
    Field::ReserveMemory,
    Field::ReserveCores,
    Field::ReserveDisk,
];

#[derive(Debug, Clone)]
pub struct ReserveDraft {
    pub revision: u64,
    pub current: Resources,
    pub envelope: Resources,
    pub minimum: Resources,
    pub current_arc: Option<ArcMode>,
    pub memory_policy: Option<HostMemoryPolicy>,
    pub automatic: bool,
    pub arc_bounds: Bound,
    pub automatic_supported: bool,
    pub automatic_maximum: Option<u64>,
    pub bounds: [Bound; 3],
}
impl ReserveDraft {
    pub fn capture(app: &App) -> anyhow::Result<Self> {
        let s = app
            .snapshot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Host capacity unavailable"))?;
        anyhow::ensure!(s.me.role == Role::Admin, "Administrator required");
        let view = s
            .host_reserves
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("This host does not support reserve editing"))?;
        anyhow::ensure!(
            view.operation.as_ref().is_none_or(|op| op.phase.terminal()),
            "Finish the current reserve change first"
        );
        let mut envelope = view.envelope;
        if let Some(memory) = &view.memory {
            envelope.memory_bytes = envelope.memory_bytes.min(memory.safe_bytes);
        }
        // A RAM shortage must not make the recovery editor inaccessible. It
        // offers no additional memory headroom, but can retain/lower reserves
        // and explicitly repair the existing cache policy.
        let available = Resources {
            memory_bytes: envelope
                .memory_bytes
                .saturating_sub(view.protected_backing.memory_bytes),
            cpu_cores: envelope
                .cpu_cores
                .checked_sub(view.protected_backing.cpu_cores)
                .ok_or_else(|| anyhow::anyhow!("CPU promises exceed capacity"))?,
            disk_bytes: envelope
                .disk_bytes
                .checked_sub(view.protected_backing.disk_bytes)
                .ok_or_else(|| anyhow::anyhow!("Disk promises exceed capacity"))?,
        };
        let minimum = view.minimum_reserves;
        let current = minimum
            .checked_add(view.active.additional)
            .map_err(anyhow::Error::msg)?;
        let available = minimum.checked_add(available).map_err(anyhow::Error::msg)?;
        let current_arc = view.active.arc.clone().or_else(|| {
            view.arc.as_ref().map(|a| ArcMode::Custom {
                max_bytes: if view.memory.as_ref().is_some_and(|m| !m.arc_ready) {
                    view.memory_policy
                        .and_then(|p| p.maximum(current.memory_bytes, &ArcMode::Automatic).ok())
                        .map(|cap| a.maximum_bytes.min(cap))
                        .unwrap_or(a.maximum_bytes)
                } else {
                    a.maximum_bytes
                },
            })
        });
        let automatic = current_arc == Some(ArcMode::Automatic);
        Ok(Self {
            revision: view.active.revision,
            current,
            minimum,
            current_arc,
            automatic,
            memory_policy: view.memory_policy,
            automatic_maximum: view.memory_policy.map(|p| p.recommended_arc_bytes),
            automatic_supported: view.memory_policy.is_some() && view.arc.is_some(),
            arc_bounds: Bound {
                min: view
                    .arc
                    .as_ref()
                    .map(|a| a.minimum_bytes / GIB + 1)
                    .unwrap_or(1)
                    .max(1),
                max: s.inventory.memory_bytes / GIB,
            },
            envelope,
            bounds: [
                Bound {
                    min: minimum.memory_bytes / GIB,
                    max: available.memory_bytes.max(current.memory_bytes) / GIB,
                },
                Bound {
                    min: minimum.cpu_cores,
                    max: available.cpu_cores,
                },
                Bound {
                    min: minimum.disk_bytes / GIB,
                    max: available.disk_bytes / GIB,
                },
            ],
        })
    }
    pub fn bound(&self, field: Field) -> Option<Bound> {
        if field == Field::ArcLimit {
            return Some(self.arc_bounds);
        }
        FIELDS
            .iter()
            .position(|f| *f == field)
            .map(|i| self.bounds[i])
    }
    pub fn values(&self, app: &App) -> anyhow::Result<Resources> {
        let mut values = [0; 3];
        for (i, field) in FIELDS.iter().enumerate() {
            values[i] = app
                .get(*field)
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Enter whole numbers for reserves."))?;
            anyhow::ensure!(
                self.bounds[i].accepts(values[i]),
                "{} reserve must be {}–{}{}; user promises are protected.",
                ["Memory", "CPU", "Disk"][i],
                self.bounds[i].min,
                self.bounds[i].max,
                if i == 1 { " cores" } else { " GiB" }
            );
        }
        Ok(Resources {
            memory_bytes: values[0]
                .checked_mul(GIB)
                .ok_or_else(|| anyhow::anyhow!("Memory overflow"))?,
            cpu_cores: values[1],
            disk_bytes: values[2]
                .checked_mul(GIB)
                .ok_or_else(|| anyhow::anyhow!("Disk overflow"))?,
        })
    }
    pub fn cache_bounds(&self, app: &App) -> Bound {
        let mut bound = self.arc_bounds;
        bound.max = self
            .memory_policy
            .and_then(|p| {
                self.values(app)
                    .ok()
                    .and_then(|v| p.allowance(v.memory_bytes).ok())
            })
            .unwrap_or(0)
            / GIB;
        bound
    }
    pub fn arc(&self, app: &App) -> anyhow::Result<Option<ArcMode>> {
        if self.current_arc.is_none() {
            return Ok(None);
        }
        let policy = self
            .memory_policy
            .ok_or_else(|| anyhow::anyhow!("Host cache budget unavailable; review host setup"))?;
        let total = self.values(app)?.memory_bytes;
        if self.automatic {
            anyhow::ensure!(
                self.automatic_supported,
                "Homecloud cache recommendation is unavailable"
            );
            policy
                .maximum(total, &ArcMode::Automatic)
                .map_err(anyhow::Error::msg)?;
            return Ok(Some(ArcMode::Automatic));
        }
        let value = app
            .get(Field::ArcLimit)
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("Enter a whole GiB cache limit"))?;
        anyhow::ensure!(
            self.arc_bounds.accepts(value),
            "Cache limit must be {}–{} GiB",
            self.arc_bounds.min,
            self.arc_bounds.max
        );
        let mode = ArcMode::Custom {
            max_bytes: value
                .checked_mul(GIB)
                .ok_or_else(|| anyhow::anyhow!("Cache limit overflow"))?,
        };
        policy.maximum(total, &mode).map_err(anyhow::Error::msg)?;
        Ok(Some(mode))
    }
}
