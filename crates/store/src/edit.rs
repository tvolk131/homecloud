use super::*;
impl Store {
    /// Claims on every new GPU and fungible resource commit with the intent.
    /// Removed GPUs/backing remain held until verified physical completion.
    pub(super) fn edit_workspace(
        &self,
        tx: &Transaction<'_>,
        vm: &mut Vm,
        changes: &WorkspaceEdit,
        restart_confirmed: bool,
    ) -> Result<bool> {
        ensure!(
            matches!(
                (vm.desired, vm.observed.as_str(), vm.has_compute()),
                (Lifecycle::Running, "running", true) | (Lifecycle::Stopped, "stopped", false)
            ),
            "Finish the current lifecycle operation before editing this workspace"
        );
        ensure!(
            !changes.requires_restart(vm) || restart_confirmed,
            "These changes require a VM restart. Confirm the restart before saving"
        );
        ensure!(
            changes.disk_gib >= vm.spec.disk_gib,
            "Shrinking VM disks is unsupported"
        );
        ensure!(
            !matches!(changes.gpu, GpuEdit::Set { .. })
                || (vm.gpus.len() <= 1 && vm.preferred_gpus.len() <= 1),
            "Use multi-GPU editing to change this workspace’s GPU selection"
        );
        let needs_host = changes.needs_host(vm);
        let target = changes.target_gpus(vm);
        let spec = changes.spec(vm);
        let configured = spec
            .resources(self.limits.disk_overhead_gib_per_vm)
            .map_err(anyhow::Error::msg)?;
        for pci in &target {
            validate_pci(pci)?;
        }
        if !target.is_empty() && !matches!(changes.gpu, GpuEdit::Keep) {
            let existing = if vm.desired == Lifecycle::Running {
                &vm.gpus
            } else {
                &vm.preferred_gpus
            };
            if target.iter().any(|pci| !existing.contains(pci)) {
                ensure!(get_user(tx, vm.owner)?.gpu_allowed, "GPU permission denied");
            }
        }
        let wanted = if vm.desired == Lifecycle::Stopped {
            configured.disk_only()
        } else {
            configured
        };
        self.admit_vm(tx, vm, wanted)?;
        if vm.desired == Lifecycle::Running {
            if changes.cores > vm.spec.cores {
                vm.cpu_ids
                    .extend(self.allocate_cores(tx, vm.id, changes.cores - vm.spec.cores)?);
            } else {
                vm.cpu_ids = self
                    .limits
                    .core_groups
                    .iter()
                    .filter(|group| group.iter().all(|cpu| vm.cpu_ids.contains(cpu)))
                    .take(changes.cores as usize)
                    .flatten()
                    .copied()
                    .collect();
            }
        }
        vm.resources = vm.resources.max(wanted);
        vm.spec = spec;
        if vm.desired == Lifecycle::Running {
            for pci in target
                .iter()
                .filter(|pci| !vm.gpus.contains(pci))
                .cloned()
                .collect::<Vec<_>>()
            {
                self.claim_gpu(tx, vm, &pci)?;
                vm.pending_gpus.push(pci);
            }
        }
        if !matches!(changes.gpu, GpuEdit::Keep) {
            vm.preferred_gpus = target;
        }
        Ok(!needs_host)
    }
}
