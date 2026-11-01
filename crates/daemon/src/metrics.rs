use super::*;
use homecloud_host::metrics::{MetricsClient, RawVmMetrics};
use std::collections::BTreeMap;
use uuid::Uuid;

const STALE_AFTER: u64 = 45;

#[derive(Default)]
pub(super) struct MetricsCache {
    configured: bool,
    attempted_at: Option<u64>,
    observations: BTreeMap<Uuid, VmMetrics>,
    previous: BTreeMap<Uuid, (RawVmMetrics, Instant)>,
}

impl MetricsCache {
    pub fn for_vms(&self, vms: &[Vm], now: u64) -> Vec<VmMetrics> {
        vms.iter()
            .map(|vm| {
                if vm.observed == "stopped" {
                    return VmMetrics::empty(vm.id, VmMetricsState::Stopped);
                }
                if vm.desired != Lifecycle::Running || vm.observed != "running" {
                    return VmMetrics::empty(vm.id, VmMetricsState::Unavailable);
                }
                let mut observation = self.observations.get(&vm.id).cloned().unwrap_or_else(|| {
                    let state = if self.configured {
                        if self.attempted_at.is_some() {
                            VmMetricsState::Unavailable
                        } else {
                            VmMetricsState::Waiting
                        }
                    } else {
                        VmMetricsState::NotConfigured
                    };
                    VmMetrics::empty(vm.id, state)
                });
                observation.attempted_at = self.attempted_at;
                if observation
                    .observed_at
                    .is_some_and(|at| now.saturating_sub(at) > STALE_AFTER)
                {
                    observation.state = VmMetricsState::Stale;
                    clear_rates(&mut observation);
                }
                observation
            })
            .collect()
    }

    fn record(
        &mut self,
        vms: &[Vm],
        result: Result<BTreeMap<Uuid, RawVmMetrics>>,
        wall: u64,
        monotonic: Instant,
    ) {
        self.configured = true;
        self.attempted_at = Some(wall);
        let running: Vec<_> = vms
            .iter()
            .filter(|vm| vm.desired == Lifecycle::Running && vm.observed == "running")
            .map(|vm| vm.id)
            .collect();
        self.observations.retain(|id, _| running.contains(id));
        self.previous.retain(|id, _| running.contains(id));
        let Ok(mut samples) = result else {
            self.previous.clear();
            for observation in self.observations.values_mut() {
                observation.state = VmMetricsState::Stale;
                clear_rates(observation);
            }
            return;
        };
        for id in running {
            let Some(sample) = samples.remove(&id) else {
                self.previous.remove(&id);
                if let Some(observation) = self.observations.get_mut(&id) {
                    observation.state = VmMetricsState::Stale;
                    clear_rates(observation);
                }
                continue;
            };
            let previous = self.previous.get(&id).filter(|(_, time)| {
                monotonic.duration_since(*time) <= Duration::from_secs(STALE_AFTER)
            });
            let elapsed = previous
                .map(|(_, time)| monotonic.duration_since(*time).as_secs_f64())
                .filter(|seconds| *seconds > 0.0);
            let prior = previous.map(|(sample, _)| sample);
            let reset = prior.is_some_and(|prior| cpu_reset(&prior.cpu, &sample.cpu));
            let cpu_percent = prior
                .zip(elapsed)
                .filter(|_| !reset)
                .and_then(|(prior, _)| cpu_percent(&prior.cpu, &sample.cpu));
            let network = sample
                .network
                .iter()
                .map(|(interface, &(received, sent))| {
                    let rates =
                        prior
                            .zip(elapsed)
                            .filter(|_| !reset)
                            .and_then(|(prior, elapsed)| {
                                let &(old_received, old_sent) = prior.network.get(interface)?;
                                Some((
                                    received.checked_sub(old_received)? as f64 / elapsed,
                                    sent.checked_sub(old_sent)? as f64 / elapsed,
                                ))
                            });
                    VmNetworkUsage {
                        interface: interface.clone(),
                        received_bytes: received,
                        sent_bytes: sent,
                        receive_bytes_per_second: rates.map(|rates| rates.0),
                        send_bytes_per_second: rates.map(|rates| rates.1),
                    }
                })
                .collect();
            self.observations.insert(
                id,
                VmMetrics {
                    vm_id: id,
                    state: VmMetricsState::Ready,
                    observed_at: Some(wall),
                    attempted_at: Some(wall),
                    cpu_percent,
                    memory: sample.memory.clone(),
                    root_filesystem: sample.root_filesystem.clone(),
                    network,
                },
            );
            self.previous.insert(id, (sample, monotonic));
        }
    }
}

fn clear_rates(observation: &mut VmMetrics) {
    observation.cpu_percent = None;
    for interface in &mut observation.network {
        interface.receive_bytes_per_second = None;
        interface.send_bytes_per_second = None;
    }
}

fn cpu_reset(
    previous: &BTreeMap<String, BTreeMap<String, f64>>,
    current: &BTreeMap<String, BTreeMap<String, f64>>,
) -> bool {
    !previous.keys().eq(current.keys())
        || current.iter().any(|(cpu, modes)| {
            let Some(prior) = previous.get(cpu) else {
                return true;
            };
            !prior.keys().eq(modes.keys()) || modes.iter().any(|(mode, value)| *value < prior[mode])
        })
}

fn cpu_percent(
    previous: &BTreeMap<String, BTreeMap<String, f64>>,
    current: &BTreeMap<String, BTreeMap<String, f64>>,
) -> Option<f64> {
    if current.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut busy = 0.0;
    for (cpu, modes) in current {
        for (mode, value) in modes {
            let delta = value - previous.get(cpu)?.get(mode)?;
            if delta < 0.0 || !delta.is_finite() {
                return None;
            }
            total += delta;
            if mode != "idle" && mode != "iowait" {
                busy += delta;
            }
        }
    }
    (total > 0.0 && total.is_finite() && busy.is_finite())
        .then(|| (100.0 * busy / total).clamp(0.0, 100.0))
}

impl Service {
    pub async fn enable_vm_metrics(&self) {
        self.metrics.write().await.configured = true;
    }

    pub async fn collect_vm_metrics_once(&self, client: &MetricsClient) -> Result<()> {
        let result = client.fetch().await;
        let error = result
            .as_ref()
            .err()
            .map(|error| anyhow::anyhow!("{error:#}"));
        let vms = self.db(|store| store.vms()).await?;
        self.metrics
            .write()
            .await
            .record(&vms, result, homecloud_store::now(), Instant::now());
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }

    pub async fn vm_metrics_unavailable(&self) -> Result<()> {
        let vms = self.db(|store| store.vms()).await?;
        self.metrics.write().await.record(
            &vms,
            Err(anyhow::anyhow!("Metrics client unavailable")),
            homecloud_store::now(),
            Instant::now(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homecloud_host::Simulator;

    fn running_vm() -> Vm {
        Vm {
            id: Uuid::new_v4(),
            owner: Uuid::new_v4(),
            spec: VmSpec {
                applications: Vec::new(),
                name: "metrics VM".into(),
                memory_gib: 4,
                cores: 1,
                disk_gib: 10,
                ssh_public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            },
            resources: Resources { memory_bytes: 4 * GIB, cpu_cores: 1, disk_bytes: 12 * GIB },
            cpu_ids: vec![2, 8],
            network_slot: 1,
            desired: Lifecycle::Running,
            observed: "running".into(),
            ssh_host_key: None,
            gpus: Vec::new(),
            preferred_gpus: Vec::new(),
            pending_gpus: Vec::new(),
            error: None,
        }
    }

    fn sample(idle: f64, user: f64, received: u64) -> RawVmMetrics {
        RawVmMetrics {
            cpu: [(
                "0".into(),
                [
                    ("idle".into(), idle),
                    ("user".into(), user),
                    ("system".into(), 0.0),
                ]
                .into(),
            )]
            .into(),
            memory: Some(VmMemoryUsage {
                total_bytes: 1000,
                available_bytes: 600,
            }),
            root_filesystem: Some(VmFilesystemUsage {
                total_bytes: 10000,
                free_bytes: 4000,
                available_bytes: 3500,
            }),
            network: [("enp5s0".into(), (received, received * 2))].into(),
        }
    }

    #[test]
    fn cpu_is_fraction_of_all_vcpus_not_physical_core_allocation() {
        let prior = sample(60.0, 40.0, 0);
        let mut next = sample(75.0, 45.0, 0);
        assert_eq!(cpu_percent(&prior.cpu, &next.cpu), Some(25.0));
        next.cpu.insert("1".into(), next.cpu["0"].clone());
        assert!(cpu_reset(&prior.cpu, &next.cpu));
        let mut prior = prior;
        prior.cpu.insert("1".into(), prior.cpu["0"].clone());
        assert_eq!(cpu_percent(&prior.cpu, &next.cpu), Some(25.0));
    }

    #[test]
    fn counters_reset_zero_delta_and_nonfinite_deltas_never_create_rates() {
        let prior = sample(60.0, 40.0, 100);
        assert!(cpu_reset(&prior.cpu, &sample(1.0, 2.0, 5).cpu));
        assert!(cpu_percent(&prior.cpu, &prior.cpu).is_none());
        assert!(cpu_percent(&prior.cpu, &sample(f64::INFINITY, 45.0, 100).cpu).is_none());
    }

    fn record_sample(
        cache: &mut MetricsCache,
        vm: &Vm,
        value: RawVmMetrics,
        wall: u64,
        time: Instant,
    ) {
        cache.record(
            std::slice::from_ref(vm),
            Ok([(vm.id, value)].into()),
            wall,
            time,
        );
    }

    fn observation(cache: &MetricsCache, vm: &Vm, now: u64) -> VmMetrics {
        cache.for_vms(std::slice::from_ref(vm), now).pop().unwrap()
    }

    fn assert_no_rates(metrics: &VmMetrics) {
        assert_eq!(metrics.cpu_percent, None);
        for network in &metrics.network {
            assert_eq!(network.receive_bytes_per_second, None);
            assert_eq!(network.send_bytes_per_second, None);
        }
    }

    fn primed_cache(vm: &Vm, time: Instant) -> MetricsCache {
        let mut cache = MetricsCache::default();
        record_sample(&mut cache, vm, sample(60.0, 40.0, 100), 100, time);
        record_sample(
            &mut cache,
            vm,
            sample(75.0, 45.0, 260),
            110,
            time + Duration::from_secs(2),
        );
        cache
    }

    #[test]
    fn configuration_and_first_failure_are_distinct_from_zero_usage() {
        let vm = running_vm();
        let mut cache = MetricsCache::default();
        assert_eq!(
            observation(&cache, &vm, 100),
            VmMetrics::empty(vm.id, VmMetricsState::NotConfigured)
        );
        cache.configured = true;
        assert_eq!(
            observation(&cache, &vm, 100),
            VmMetrics::empty(vm.id, VmMetricsState::Waiting)
        );
        cache.record(
            std::slice::from_ref(&vm),
            Err(anyhow::anyhow!("offline")),
            100,
            Instant::now(),
        );
        let metrics = observation(&cache, &vm, 100);
        assert_eq!(metrics.state, VmMetricsState::Unavailable);
        assert_eq!(metrics.attempted_at, Some(100));
        assert_eq!(metrics.observed_at, None);
        assert_eq!(metrics.memory, None);
        assert_no_rates(&metrics);
    }

    #[test]
    fn first_sample_keeps_guest_gauges_and_counters_without_rates() {
        let vm = running_vm();
        let mut cache = MetricsCache::default();
        record_sample(
            &mut cache,
            &vm,
            sample(60.0, 40.0, 100),
            100,
            Instant::now(),
        );
        let metrics = observation(&cache, &vm, 100);
        assert_eq!(metrics.state, VmMetricsState::Ready);
        assert_eq!(metrics.observed_at, Some(100));
        assert_eq!(metrics.attempted_at, Some(100));
        assert_eq!(metrics.memory.unwrap().used_bytes(), 400);
        assert_eq!(metrics.root_filesystem.unwrap().used_bytes(), 6000);
        assert_eq!(metrics.network[0].received_bytes, 100);
        assert_eq!(metrics.network[0].sent_bytes, 200);
        assert_no_rates(&observation(&cache, &vm, 100));
    }

    #[test]
    fn rates_use_fractional_monotonic_time_even_if_wall_clock_moves_backwards() {
        let vm = running_vm();
        let mut cache = MetricsCache::default();
        let time = Instant::now();
        record_sample(&mut cache, &vm, sample(60.0, 40.0, 100), 100, time);
        record_sample(
            &mut cache,
            &vm,
            sample(75.0, 45.0, 260),
            90,
            time + Duration::from_millis(2500),
        );
        let metrics = observation(&cache, &vm, 90);
        assert_eq!(metrics.state, VmMetricsState::Ready);
        assert_eq!(metrics.observed_at, Some(90));
        assert_eq!(metrics.cpu_percent, Some(25.0));
        assert_eq!(metrics.network[0].receive_bytes_per_second, Some(64.0));
        assert_eq!(metrics.network[0].send_bytes_per_second, Some(128.0));
    }

    #[test]
    fn stale_reads_preserve_gauges_and_timestamps_but_clear_rates() {
        let vm = running_vm();
        let cache = primed_cache(&vm, Instant::now());
        let ready = observation(&cache, &vm, 110 + STALE_AFTER);
        assert_eq!(ready.state, VmMetricsState::Ready);
        assert_eq!(ready.cpu_percent, Some(25.0));
        let stale = observation(&cache, &vm, 111 + STALE_AFTER);
        assert_eq!(stale.state, VmMetricsState::Stale);
        assert_eq!(stale.observed_at, ready.observed_at);
        assert_eq!(stale.attempted_at, ready.attempted_at);
        assert_eq!(stale.memory, ready.memory);
        assert_eq!(stale.root_filesystem, ready.root_filesystem);
        assert_eq!(
            stale.network[0].received_bytes,
            ready.network[0].received_bytes
        );
        assert_no_rates(&stale);
        assert_eq!(observation(&cache, &vm, 110).cpu_percent, Some(25.0));
    }

    #[test]
    fn failed_collection_retains_last_success_and_recovery_needs_a_new_baseline() {
        let vm = running_vm();
        let time = Instant::now();
        let mut cache = primed_cache(&vm, time);
        let ready = observation(&cache, &vm, 110);
        cache.record(
            std::slice::from_ref(&vm),
            Err(anyhow::anyhow!("offline")),
            120,
            time + Duration::from_secs(4),
        );
        let stale = observation(&cache, &vm, 120);
        assert_eq!(stale.state, VmMetricsState::Stale);
        assert_eq!(stale.observed_at, Some(110));
        assert_eq!(stale.attempted_at, Some(120));
        assert_eq!(stale.memory, ready.memory);
        assert_eq!(stale.root_filesystem, ready.root_filesystem);
        assert_no_rates(&stale);
        assert!(cache.previous.is_empty());
        record_sample(
            &mut cache,
            &vm,
            sample(90.0, 50.0, 300),
            121,
            time + Duration::from_secs(5),
        );
        assert_no_rates(&observation(&cache, &vm, 121));
        record_sample(
            &mut cache,
            &vm,
            sample(105.0, 55.0, 400),
            122,
            time + Duration::from_secs(6),
        );
        assert_eq!(
            observation(&cache, &vm, 122).network[0].receive_bytes_per_second,
            Some(100.0)
        );
    }

    #[test]
    fn missing_guest_is_unavailable_or_stale_and_never_uses_another_guests_sample() {
        let vm = running_vm();
        let other = running_vm();
        let time = Instant::now();
        let mut cache = MetricsCache::default();
        cache.record(
            std::slice::from_ref(&vm),
            Ok([(other.id, sample(1.0, 2.0, 9))].into()),
            100,
            time,
        );
        assert_eq!(
            observation(&cache, &vm, 100).state,
            VmMetricsState::Unavailable
        );
        assert!(cache.observations.is_empty());
        record_sample(
            &mut cache,
            &vm,
            sample(60.0, 40.0, 100),
            110,
            time + Duration::from_secs(1),
        );
        cache.record(
            std::slice::from_ref(&vm),
            Ok(BTreeMap::new()),
            120,
            time + Duration::from_secs(2),
        );
        let stale = observation(&cache, &vm, 120);
        assert_eq!(stale.state, VmMetricsState::Stale);
        assert_eq!(stale.observed_at, Some(110));
        assert_eq!(stale.attempted_at, Some(120));
        assert!(stale.memory.is_some());
        assert_no_rates(&stale);
        record_sample(
            &mut cache,
            &vm,
            sample(90.0, 50.0, 300),
            130,
            time + Duration::from_secs(3),
        );
        assert_no_rates(&observation(&cache, &vm, 130));
    }

    #[test]
    fn stopped_pending_deleted_and_removed_guests_do_not_expose_old_metrics() {
        let mut vm = running_vm();
        let time = Instant::now();
        let mut cache = primed_cache(&vm, time);
        for (desired, observed, state) in [
            (Lifecycle::Stopped, "running", VmMetricsState::Unavailable),
            (Lifecycle::Running, "stopped", VmMetricsState::Stopped),
            (Lifecycle::Running, "pending", VmMetricsState::Unavailable),
            (Lifecycle::Running, "unknown", VmMetricsState::Unavailable),
            (
                Lifecycle::Running,
                "unavailable",
                VmMetricsState::Unavailable,
            ),
            (Lifecycle::Deleted, "deleted", VmMetricsState::Unavailable),
        ] {
            vm.desired = desired;
            vm.observed = observed.into();
            assert_eq!(
                observation(&cache, &vm, 120),
                VmMetrics::empty(vm.id, state)
            );
        }
        cache.record(
            std::slice::from_ref(&vm),
            Ok([(vm.id, sample(90.0, 50.0, 300))].into()),
            120,
            time + Duration::from_secs(3),
        );
        assert!(cache.observations.is_empty());
        assert!(cache.previous.is_empty());
        vm.desired = Lifecycle::Running;
        vm.observed = "running".into();
        record_sample(
            &mut cache,
            &vm,
            sample(90.0, 50.0, 300),
            130,
            time + Duration::from_secs(4),
        );
        assert_no_rates(&observation(&cache, &vm, 130));
        cache.record(&[], Ok(BTreeMap::new()), 140, time + Duration::from_secs(5));
        assert!(cache.observations.is_empty());
        assert!(cache.previous.is_empty());
        assert!(cache.for_vms(&[], 140).is_empty());
    }

    #[test]
    fn zero_elapsed_and_stale_baselines_do_not_generate_rates() {
        let vm = running_vm();
        let time = Instant::now();
        for elapsed in [
            Duration::ZERO,
            Duration::from_millis(STALE_AFTER * 1000 + 1),
        ] {
            let mut cache = MetricsCache::default();
            record_sample(&mut cache, &vm, sample(60.0, 40.0, 100), 100, time);
            record_sample(
                &mut cache,
                &vm,
                sample(75.0, 45.0, 260),
                110,
                time + elapsed,
            );
            assert_no_rates(&observation(&cache, &vm, 110));
        }
        let mut cache = MetricsCache::default();
        record_sample(&mut cache, &vm, sample(60.0, 40.0, 100), 100, time);
        record_sample(
            &mut cache,
            &vm,
            sample(75.0, 45.0, 550),
            110,
            time + Duration::from_secs(STALE_AFTER),
        );
        assert_eq!(
            observation(&cache, &vm, 110).network[0].receive_bytes_per_second,
            Some(10.0)
        );
    }

    #[test]
    fn cpu_counter_reset_or_hotplug_invalidates_rates_until_next_sample() {
        let vm = running_vm();
        let time = Instant::now();
        for change in [
            "reset",
            "cpu_added",
            "cpu_removed",
            "mode_added",
            "mode_removed",
        ] {
            let mut prior = sample(60.0, 40.0, 100);
            let mut next = sample(75.0, 45.0, 260);
            match change {
                "reset" => next.cpu.get_mut("0").unwrap().insert("user".into(), 1.0),
                "cpu_added" => {
                    next.cpu.insert("1".into(), next.cpu["0"].clone());
                    None
                }
                "cpu_removed" => {
                    prior.cpu.insert("1".into(), prior.cpu["0"].clone());
                    None
                }
                "mode_added" => next.cpu.get_mut("0").unwrap().insert("iowait".into(), 1.0),
                _ => prior.cpu.get_mut("0").unwrap().insert("iowait".into(), 1.0),
            };
            let mut cache = MetricsCache::default();
            record_sample(&mut cache, &vm, prior, 100, time);
            record_sample(
                &mut cache,
                &vm,
                next.clone(),
                110,
                time + Duration::from_secs(1),
            );
            let metrics = observation(&cache, &vm, 110);
            assert_eq!(metrics.state, VmMetricsState::Ready);
            assert!(metrics.memory.is_some());
            assert_no_rates(&metrics);
            next.cpu
                .get_mut("0")
                .unwrap()
                .entry("idle".into())
                .and_modify(|value| *value += 1.0);
            next.network.get_mut("enp5s0").unwrap().0 += 100;
            record_sample(&mut cache, &vm, next, 120, time + Duration::from_secs(2));
            let metrics = observation(&cache, &vm, 120);
            assert_eq!(metrics.cpu_percent, Some(0.0));
            assert_eq!(metrics.network[0].receive_bytes_per_second, Some(100.0));
        }
    }

    #[test]
    fn interface_hotplug_and_counter_reset_are_scoped_to_that_interface() {
        let vm = running_vm();
        let time = Instant::now();
        for counters in [(50, 700), (400, 50)] {
            let mut cache = MetricsCache::default();
            let mut prior = sample(60.0, 40.0, 100);
            prior.network.insert("removed".into(), (100, 200));
            prior.network.insert("stable".into(), (100, 200));
            let mut next = sample(75.0, 45.0, 260);
            next.network.insert("enp5s0".into(), counters);
            next.network.insert("new".into(), (9999, 8888));
            next.network.insert("stable".into(), (200, 400));
            record_sample(&mut cache, &vm, prior, 100, time);
            record_sample(&mut cache, &vm, next, 110, time + Duration::from_secs(2));
            let metrics = observation(&cache, &vm, 110);
            assert_eq!(metrics.cpu_percent, Some(25.0));
            assert_eq!(metrics.network.len(), 3);
            for interface in &metrics.network {
                if interface.interface == "stable" {
                    assert_eq!(interface.receive_bytes_per_second, Some(50.0));
                    assert_eq!(interface.send_bytes_per_second, Some(100.0));
                } else {
                    assert_eq!(interface.receive_bytes_per_second, None);
                    assert_eq!(interface.send_bytes_per_second, None);
                }
            }
        }
    }

    #[test]
    fn idle_iowait_and_zero_cpu_delta_do_not_inflate_cpu_usage() {
        let mut prior = sample(60.0, 40.0, 100);
        let mut next = sample(75.0, 45.0, 260);
        prior.cpu.get_mut("0").unwrap().insert("iowait".into(), 5.0);
        next.cpu.get_mut("0").unwrap().insert("iowait".into(), 10.0);
        assert_eq!(cpu_percent(&prior.cpu, &next.cpu), Some(20.0));
        let vm = running_vm();
        let time = Instant::now();
        let mut cache = MetricsCache::default();
        next.cpu = prior.cpu.clone();
        record_sample(&mut cache, &vm, prior, 100, time);
        record_sample(&mut cache, &vm, next, 110, time + Duration::from_secs(2));
        let metrics = observation(&cache, &vm, 110);
        assert_eq!(metrics.cpu_percent, None);
        assert_eq!(metrics.network[0].receive_bytes_per_second, Some(80.0));
    }

    #[tokio::test]
    async fn snapshots_scope_cached_metrics_to_visible_vms_for_each_user() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
        let (_, token) = store.bootstrap("Administrator").unwrap();
        store.enroll(&token, "admin").unwrap();
        let mut vm_ids = BTreeMap::new();
        for peer in ["alice", "bob"] {
            let (token, _, _) = store
                .invite("admin", peer, Role::User, running_vm().resources, false, 1)
                .unwrap();
            store.enroll(&token, peer).unwrap();
            let operation = store
                .submit(
                    peer,
                    Uuid::new_v4(),
                    Mutation::Create {
                        spec: running_vm().spec,
                    },
                )
                .unwrap();
            vm_ids.insert(peer, operation.vm_id);
        }
        let service = Service::new(
            store,
            Arc::new(Simulator::new(directory.path().join("observed.json"))),
        )
        .unwrap();
        service.reconcile_once().await.unwrap();
        let vms = service.store.vms().unwrap();
        assert_eq!(vms.len(), 2);
        assert!(vms.iter().all(|vm| vm.observed == "running"));
        let time = Instant::now();
        let wall = homecloud_store::now();
        let first = vm_ids
            .values()
            .map(|id| (*id, sample(60.0, 40.0, 100)))
            .collect();
        let next = vm_ids
            .values()
            .map(|id| (*id, sample(75.0, 45.0, 260)))
            .collect();
        {
            let mut cache = service.metrics.write().await;
            cache.record(&vms, Ok(first), wall, time);
            cache.record(&vms, Ok(next), wall, time + Duration::from_secs(2));
        }
        for peer in ["alice", "bob", "admin"] {
            let Response::Snapshot(snapshot) = service
                .request(peer.into(), Request::Snapshot)
                .await
                .unwrap()
            else {
                panic!("Expected snapshot")
            };
            let expected: std::collections::BTreeSet<_> = if peer == "admin" {
                vm_ids.values().copied().collect()
            } else {
                [vm_ids[peer]].into()
            };
            assert_eq!(
                snapshot
                    .vms
                    .iter()
                    .map(|vm| vm.id)
                    .collect::<std::collections::BTreeSet<_>>(),
                expected
            );
            assert_eq!(
                snapshot
                    .vm_metrics
                    .iter()
                    .map(|metrics| metrics.vm_id)
                    .collect::<std::collections::BTreeSet<_>>(),
                expected
            );
            assert_eq!(snapshot.vm_metrics.len(), expected.len());
            assert!(
                snapshot
                    .vm_metrics
                    .iter()
                    .all(|metrics| metrics.state == VmMetricsState::Ready
                        && metrics.cpu_percent == Some(25.0))
            );
        }
    }
}
