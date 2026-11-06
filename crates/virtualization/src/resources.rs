use anyhow::{Context, Result, ensure};
use homecloud_core::*;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct Resource {
    #[serde(rename = "type")]
    kind: String,
    node: Option<String>,
    vmid: Option<u32>,
    name: Option<String>,
    storage: Option<String>,
    status: Option<String>,
    maxcpu: Option<u32>,
    cpu: Option<f64>,
    maxmem: Option<u64>,
    mem: Option<u64>,
    maxdisk: Option<u64>,
    disk: Option<u64>,
    template: Option<serde_json::Value>,
}

pub fn parse_proxmox_resources(data: serde_json::Value) -> Result<VirtualizationInventory> {
    let resources: Vec<Resource> =
        serde_json::from_value(data).context("Invalid Proxmox resource inventory")?;
    let mut inventory = VirtualizationInventory {
        source: VirtualizationSource::Proxmox,
        nodes: vec![],
        guests: vec![],
        storage: vec![],
    };
    let mut identities = BTreeSet::new();
    for resource in resources {
        if !matches!(resource.kind.as_str(), "node" | "qemu" | "lxc" | "storage") {
            continue;
        }
        let node = resource.node.context("Proxmox resource has no node")?;
        ensure!(!node.is_empty(), "Proxmox resource has an empty node");
        let status = resource.status.unwrap_or_else(|| "unknown".into());
        match resource.kind.as_str() {
            "node" => {
                ensure!(
                    identities.insert(format!("node/{node}")),
                    "Duplicate Proxmox node"
                );
                if let Some(ratio) = resource.cpu {
                    ensure!(
                        ratio.is_finite() && (0.0..=1.0).contains(&ratio),
                        "Invalid Proxmox CPU usage ratio"
                    );
                }
                inventory.nodes.push(VirtualizationNode {
                    name: node,
                    status,
                    cpu_threads: resource.maxcpu,
                    cpu_usage_ratio: resource.cpu,
                    memory_capacity_bytes: resource.maxmem,
                    memory_used_bytes: resource.mem,
                });
            }
            "qemu" | "lxc" => {
                let vmid = resource.vmid.context("Proxmox guest has no VM ID")?;
                ensure!(vmid > 0, "Invalid Proxmox VM ID");
                ensure!(
                    identities.insert(format!("guest/{vmid}")),
                    "Duplicate Proxmox guest"
                );
                let template = match resource.template {
                    None | Some(serde_json::Value::Null) => false,
                    Some(serde_json::Value::Bool(value)) => value,
                    Some(value) if value == 0 => false,
                    Some(value) if value == 1 => true,
                    Some(_) => anyhow::bail!("Invalid Proxmox template flag"),
                };
                inventory.guests.push(VirtualizationGuest {
                    node,
                    vmid,
                    kind: if resource.kind == "qemu" {
                        VirtualizationGuestKind::VirtualMachine
                    } else {
                        VirtualizationGuestKind::Container
                    },
                    name: resource.name,
                    status,
                    template,
                    cpu_limit: resource.maxcpu,
                    memory_limit_bytes: resource.maxmem,
                    memory_used_bytes: resource.mem,
                    reported_disk_capacity_bytes: resource.maxdisk,
                });
            }
            "storage" => {
                let name = resource.storage.context("Proxmox storage has no name")?;
                ensure!(!name.is_empty(), "Proxmox storage has an empty name");
                ensure!(
                    identities.insert(format!("storage/{node}/{name}")),
                    "Duplicate Proxmox storage"
                );
                inventory.storage.push(VirtualizationStorage {
                    node,
                    name,
                    status,
                    capacity_bytes: resource.maxdisk,
                    used_bytes: resource.disk,
                });
            }
            _ => unreachable!(),
        }
    }
    inventory
        .nodes
        .sort_by(|left, right| left.name.cmp(&right.name));
    inventory.guests.sort_by_key(|guest| guest.vmid);
    inventory
        .storage
        .sort_by(|left, right| (&left.node, &left.name).cmp(&(&right.node, &right.name)));
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_native_ids_templates_and_unknown_usage_without_tenant_grants() {
        let inventory = parse_proxmox_resources(json!([
            {"type":"qemu","node":"pve","vmid":9000,"template":1,"maxdisk":1024,"disk":0},
            {"type":"node","node":"pve","status":"online","maxcpu":16,"cpu":0.5},
            {"type":"lxc","node":"pve","vmid":101,"template":false,"status":"running"},
            {"type":"storage","node":"pve","storage":"local-zfs","maxdisk":2000,"disk":1000},
            {"type":"future-resource","extra":true}
        ]))
        .unwrap();
        assert_eq!(inventory.source, VirtualizationSource::Proxmox);
        assert_eq!(inventory.guests[0].vmid, 101);
        assert_eq!(inventory.guests[0].kind, VirtualizationGuestKind::Container);
        assert_eq!(inventory.guests[0].memory_used_bytes, None);
        assert!(inventory.guests[1].template);
        assert_eq!(inventory.guests[1].reported_disk_capacity_bytes, Some(1024));
        assert_eq!(inventory.storage[0].used_bytes, Some(1000));
        assert_eq!(inventory.nodes[0].cpu_threads, Some(16));
    }

    #[test]
    fn rejects_ambiguous_or_malformed_known_resources() {
        for data in [
            json!([{"type":"qemu","vmid":1}]),
            json!([{"type":"qemu","node":"pve"}]),
            json!([{"type":"qemu","node":"pve","vmid":0}]),
            json!([{"type":"qemu","node":"pve","vmid":1,"template":2}]),
            json!([{"type":"node","node":"pve","cpu":2}]),
            json!([{"type":"storage","node":"pve"}]),
            json!([{"type":"node","node":"pve"},{"type":"node","node":"pve"}]),
            json!([{"type":"qemu","node":"pve","vmid":1},{"type":"lxc","node":"other","vmid":1}]),
        ] {
            assert!(parse_proxmox_resources(data).is_err());
        }
    }

    #[test]
    fn accepts_empty_inventory_without_inventing_capacity() {
        let inventory = parse_proxmox_resources(json!([])).unwrap();
        assert!(inventory.nodes.is_empty());
        assert!(inventory.guests.is_empty());
        assert!(inventory.storage.is_empty());
    }
}
