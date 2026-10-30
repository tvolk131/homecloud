use super::*;
use homecloud_core::GIB;
use serde_json::{Value, json};
use std::{io::Write, os::unix::fs::symlink, process::Command, sync::Mutex};

#[derive(Default)]
struct FakeCommands {
    responses: BTreeMap<Vec<String>, Value>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl FakeCommands {
    fn add(&mut self, argv: &[&str], stdout: Value, success: bool) {
        self.responses.insert(argv.iter().map(|arg| (*arg).into()).collect(), json!({
            "stdout": if stdout.is_string() { stdout.as_str().unwrap().to_owned() } else { stdout.to_string() },
            "success":success, "stderr": if success { "" } else { "fixture unavailable" }
        }));
    }
}

#[async_trait]
impl Commands for FakeCommands {
    async fn run(&self, program: &str, args: &[&str], _: u64) -> Result<CommandOutput> {
        ensure!(
            Path::new(program).is_absolute(),
            "Commands must use fixed absolute paths"
        );
        let argv: Vec<_> =
            std::iter::once(Path::new(program).file_name().unwrap().to_str().unwrap())
                .chain(args.iter().copied())
                .map(String::from)
                .collect();
        self.calls.lock().unwrap().push(argv.clone());
        let response = self
            .responses
            .get(&argv)
            .with_context(|| format!("Unexpected command {argv:?}"))?;
        Ok(CommandOutput {
            success: response["success"].as_bool().unwrap(),
            stdout: response["stdout"].as_str().unwrap().into(),
            stderr: response["stderr"].as_str().unwrap().into(),
        })
    }
}

fn vm() -> Vm {
    serde_json::from_value(json!({
        "id":"00000000-0000-4000-8000-000000000001",
        "owner":"00000000-0000-4000-8000-000000000002",
        "spec":{"name":"fixture","memory_gib":4,"cores":1,"disk_gib":10,"ssh_public_key":"fixture"},
        "resources":{"memory_bytes":4*GIB,"cpu_cores":1,"disk_bytes":12*GIB},
        "cpu_ids":[4,10],"network_slot":1,"desired":"running","observed":"running",
        "ssh_host_key":null,"gpus":["0000:01:00.0"],"error":null
    }))
    .unwrap()
}

fn instance(vm: &Vm) -> Value {
    json!({"name":vm.incus_name(),"status":"Running", "config":{
        "user.homecloud.id":vm.id,"user.homecloud.owner":vm.owner,
        "limits.cpu":"4,10","limits.memory":"4GiB","boot.autostart":"false"
    },"devices":{"root":{"type":"disk","size":"10GiB"},
        "gpu0":{"type":"gpu","pci":"0000:01:00.0"}}})
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, data) in [
        ("proc/cpuinfo", "model name\t: Fixture CPU\n"),
        ("proc/meminfo", "MemTotal: 8388608 kB\n"),
        ("proc/sys/kernel/hostname", "fixture-host\n"),
        ("sys/class/dmi/id/board_name", "Fixture Board\n"),
        ("sys/firmware/acpi/tables/DMAR", "fixture"),
        (
            "sys/devices/system/cpu/cpu0/topology/thread_siblings_list",
            "0,1\n",
        ),
        (
            "sys/devices/system/cpu/cpu1/topology/thread_siblings_list",
            "0,1\n",
        ),
        (
            "sys/devices/system/cpu/cpu2/topology/thread_siblings_list",
            "2,3\n",
        ),
        ("sys/bus/pci/devices/0000:01:00.0/class", "0x030000\n"),
        ("sys/bus/pci/devices/0000:01:00.0/boot_vga", "0\n"),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    let group = dir.path().join("sys/kernel/iommu_groups/12");
    std::fs::create_dir_all(group.join("devices")).unwrap();
    let gpu = dir.path().join("sys/bus/pci/devices/0000:01:00.0");
    symlink(&group, gpu.join("iommu_group")).unwrap();
    symlink(&gpu, group.join("devices/0000:01:00.0")).unwrap();
    dir
}

fn inventory_commands(instances: Value) -> FakeCommands {
    let mut commands = FakeCommands::default();
    commands.add(
        &["incus", "query", "-X", "GET", "/1.0/instances?recursion=1"],
        instances,
        true,
    );
    commands.add(
        &["lspci", "-s", "0000:01:00.0"],
        json!("Fixture GPU\n"),
        true,
    );
    commands.add(
        &["zfs", "get", "-Hp", "-o", "value", "available", "test"],
        json!("10737418240\n"),
        true,
    );
    commands.add(
        &["zpool", "list", "-H", "-o", "health", "test"],
        json!("ONLINE\n"),
        true,
    );
    commands
}

fn policy() -> Value {
    json!({"pool":"test","minimum_pool_available_bytes":2*GIB,
        "capacity":{"memory_bytes":8*GIB,"cpu_cores":2,"disk_bytes":20*GIB},
        "allowed_gpus":["0000:01:00.0"]})
}

/// Use the existing Python implementation as a temporary compatibility oracle.
/// Both implementations see the same private files and fake command responses.
fn python_reference(observer: &Observer<FakeCommands>, kind: &str, data: Value) -> Value {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("host_runtime.py");
    let responses: Vec<_> = observer
        .commands
        .responses
        .iter()
        .map(|(argv, result)| json!({"argv":argv,"result":result}))
        .collect();
    let script = r#"
import importlib.util,json,sys
from pathlib import Path
from types import SimpleNamespace
source,root,kind=sys.argv[1:]
spec=importlib.util.spec_from_file_location('reference_host',source)
host=importlib.util.module_from_spec(spec);sys.modules[spec.name]=host;spec.loader.exec_module(host)
payload=json.load(sys.stdin)
host.Path=lambda p: Path(root)/str(p).lstrip('/')
host.STATE=Path(root)/'var/lib/homecloud/host'
host.os.uname=lambda: SimpleNamespace(nodename=(Path(root)/'proc/sys/kernel/hostname').read_text().strip())
def run(*argv,check=True,**kwargs):
 result=next(r['result'] for r in payload['responses'] if r['argv']==list(argv))
 if check and not result['success']: raise RuntimeError('fixture unavailable')
 return SimpleNamespace(stdout=result['stdout'],stderr=result['stderr'],returncode=0 if result['success'] else 1)
host.run=run
print(json.dumps(host.inventory(payload['data']) if kind=='inventory' else host.observe(payload['data'])))
"#;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(source)
        .arg(&observer.root)
        .arg(kind)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"responses":responses,"data":data})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn observation_matches_python_for_ownership_drift_missing_and_gpu_states() {
    let root = fixture();
    let base = vm();
    let mut scenarios = vec![(base.clone(), vec![instance(&base)])];
    scenarios.push((base.clone(), vec![]));
    for (field, value) in [
        (
            "user.homecloud.owner",
            "00000000-0000-4000-8000-000000000003",
        ),
        ("limits.cpu", "4"),
        ("limits.memory", "8GiB"),
    ] {
        let mut record = instance(&base);
        record["config"][field] = json!(value);
        scenarios.push((base.clone(), vec![record]));
    }
    let mut stopped = instance(&base);
    stopped["status"] = json!("Stopped");
    scenarios.push((base.clone(), vec![stopped]));
    let mut inherited = instance(&base);
    inherited["expanded_devices"] = inherited["devices"].clone();
    inherited["expanded_devices"]["gpu1"] = json!({"type":"gpu","pci":"0000:02:00.0"});
    scenarios.push((base.clone(), vec![inherited]));
    let mut duplicate = instance(&base);
    duplicate["devices"]["gpu1"] = duplicate["devices"]["gpu0"].clone();
    scenarios.push((base.clone(), vec![duplicate]));
    let mut invalid_name = instance(&base);
    invalid_name["devices"]["foreign"] = invalid_name["devices"]["gpu0"].take();
    invalid_name["devices"]
        .as_object_mut()
        .unwrap()
        .remove("gpu0");
    scenarios.push((base.clone(), vec![invalid_name]));
    let mut no_compute = base.clone();
    no_compute.cpu_ids.clear();
    no_compute.gpus.clear();
    no_compute.desired = Lifecycle::Stopped;
    let mut record = instance(&no_compute);
    record["status"] = json!("Stopped");
    record["devices"].as_object_mut().unwrap().remove("gpu0");
    scenarios.push((no_compute.clone(), vec![record.clone()]));
    record["config"]["boot.autostart"] = json!("true");
    scenarios.push((no_compute, vec![record]));
    let mut deleted = base.clone();
    deleted.observed = "deleted".into();
    scenarios.push((deleted, vec![]));

    let progress = root.path().join(format!(
        "var/lib/homecloud/host/setup-{}.json",
        base.id.simple()
    ));
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(progress, r#"{"stage":"guest_agent","since":123}"#).unwrap();
    for (vm, records) in scenarios {
        let observer = Observer {
            root: root.path().into(),
            commands: inventory_commands(json!(records)),
        };
        let expected = python_reference(&observer, "observe", json!([vm.clone()]));
        let observed = observer.observe(vec![vm]).await.unwrap();
        assert_eq!(serde_json::to_value(observed).unwrap(), expected);
    }
}

#[tokio::test]
async fn unavailable_incus_does_not_report_missing_or_release_claims() {
    let mut commands = inventory_commands(json!([]));
    commands.add(
        &["incus", "query", "-X", "GET", "/1.0/instances?recursion=1"],
        json!(""),
        false,
    );
    let observer = Observer {
        root: "/does-not-exist".into(),
        commands,
    };
    let observed = observer.observe(vec![vm()]).await.unwrap();
    assert_eq!(observed[0].state, "unavailable");
    assert!(
        observed[0]
            .error
            .as_ref()
            .unwrap()
            .contains("allocations remain held")
    );
    assert!(observed[0].progress.is_none());
}

#[tokio::test]
async fn inventory_matches_python_for_available_assigned_and_reserved_gpus() {
    for assigned in [false, true] {
        for display in [false, true] {
            let root = fixture();
            std::fs::write(
                root.path()
                    .join("sys/bus/pci/devices/0000:01:00.0/boot_vga"),
                if display { "1" } else { "0" },
            )
            .unwrap();
            let mut record = instance(&vm());
            record["expanded_devices"] = record["devices"].clone();
            let mut commands =
                inventory_commands(if assigned { json!([record]) } else { json!([]) });
            commands.add(
                &[
                    "incus",
                    "exec",
                    &vm().incus_name(),
                    "--",
                    "nvidia-smi",
                    "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
                    "--format=csv,noheader,nounits",
                ],
                json!("10, 1024, 8192, 42\n"),
                true,
            );
            let observer = Observer {
                root: root.path().into(),
                commands,
            };
            let expected = python_reference(&observer, "inventory", policy());
            let observed = observer
                .inventory_with_policy(serde_json::from_value(policy()).unwrap())
                .await
                .unwrap();
            assert_eq!(serde_json::to_value(observed).unwrap(), expected);
        }
    }
}

#[tokio::test]
async fn missing_iommu_and_unavailable_pool_are_reported_without_mutation() {
    let root = fixture();
    std::fs::remove_file(
        root.path()
            .join("sys/bus/pci/devices/0000:01:00.0/iommu_group"),
    )
    .unwrap();
    std::fs::remove_dir_all(root.path().join("sys/kernel/iommu_groups/12")).unwrap();
    let mut commands = inventory_commands(json!([]));
    commands.add(
        &["zfs", "get", "-Hp", "-o", "value", "available", "test"],
        json!(""),
        false,
    );
    let observer = Observer {
        root: root.path().into(),
        commands,
    };
    let inventory = observer
        .inventory_with_policy(serde_json::from_value(policy()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        inventory.iommu.unwrap().state,
        "needs_firmware_configuration"
    );
    assert_eq!(
        inventory.gpus[0].readiness.state,
        "needs_firmware_configuration"
    );
    assert_eq!(inventory.pool_available_bytes, 0);
    assert_eq!(inventory.pool_health, "unavailable");
    assert!(
        inventory
            .warnings
            .iter()
            .any(|warning| warning.contains("growth is blocked"))
    );
}

#[tokio::test]
async fn observer_rejects_non_observation_requests_without_running_commands() {
    let observer = Observer {
        root: "/does-not-exist".into(),
        commands: FakeCommands::default(),
    };
    for request in [
        HostRequest::ReserveStatus,
        HostRequest::Backing { users: vec![] },
    ] {
        assert!(
            observer
                .request(request)
                .await
                .unwrap_err()
                .to_string()
                .contains("only supports")
        );
    }
    assert!(observer.commands.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn guest_command_output_is_bounded_while_reading() {
    assert_eq!(read_output(&b"bounded"[..]).await.unwrap(), b"bounded");
    let oversized = vec![b'x'; MAX_OUTPUT + 1];
    assert!(
        read_output(oversized.as_slice())
            .await
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
}
