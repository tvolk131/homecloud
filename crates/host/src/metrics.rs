use anyhow::{Context, Result, bail, ensure};
use homecloud_core::{VmFilesystemUsage, VmMemoryUsage};
use reqwest::{Client, Url, redirect::Policy};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

pub const MAX_METRICS_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub port: u16,
    pub server_name: String,
    pub client_identity: PathBuf,
    pub server_certificate: PathBuf,
}

#[derive(Debug)]
pub struct MetricsClient {
    client: Client,
    url: Url,
}

impl MetricsClient {
    pub fn new(config: &MetricsConfig) -> Result<Self> {
        ensure!(config.port != 0, "Invalid metrics port");
        ensure!(
            !config.server_name.is_empty()
                && config.server_name.len() <= 253
                && config
                    .server_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                && config.server_name.parse::<std::net::IpAddr>().is_err(),
            "Metrics server name must be a DNS name in the trusted Incus certificate"
        );
        let url = Url::parse(&format!(
            "https://{}:{}/1.0/metrics",
            config.server_name, config.port
        ))?;
        ensure!(
            url.host_str()
                .is_some_and(|host| host.parse::<std::net::IpAddr>().is_err()),
            "Metrics server name must remain a DNS name after URL normalization"
        );
        let server = std::fs::read(&config.server_certificate)
            .context("Reading trusted Incus certificate")?;
        let identity =
            std::fs::read(&config.client_identity).context("Reading metrics client identity")?;
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from_pem_slice(&server)?)?;
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_client_auth_cert(
                    CertificateDer::pem_slice_iter(&identity)
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                    PrivateKeyDer::from_pem_slice(&identity)?,
                )?;
        let client = Client::builder()
            .tls_backend_preconfigured(tls)
            .resolve(
                url.host_str().context("Missing metrics server name")?,
                SocketAddr::from((Ipv4Addr::LOCALHOST, config.port)),
            )
            .no_proxy()
            .redirect(Policy::none())
            .https_only(true)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { client, url })
    }

    pub async fn fetch(&self) -> Result<BTreeMap<Uuid, RawVmMetrics>> {
        let mut response = self
            .client
            .get(self.url.clone())
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.status().is_success(),
            "Unexpected Incus metrics HTTP status: {}",
            response.status()
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_METRICS_BYTES as u64),
            "Incus metrics response exceeds limit"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= MAX_METRICS_BYTES,
                "Incus metrics response exceeds limit"
            );
            body.extend_from_slice(&chunk);
        }
        let mut text = String::with_capacity(body.len());
        for line in body.split_inclusive(|byte| *byte == b'\n') {
            if let Ok(line) = std::str::from_utf8(line) {
                text.push_str(line);
            }
        }
        parse(&text)
    }
}

#[derive(Clone, Debug, Default)]
pub struct RawVmMetrics {
    pub cpu: BTreeMap<String, BTreeMap<String, f64>>,
    pub memory: Option<VmMemoryUsage>,
    pub root_filesystem: Option<VmFilesystemUsage>,
    pub network: BTreeMap<String, (u64, u64)>,
}

#[derive(Default)]
struct PendingVm {
    invalid: bool,
    cpu: BTreeMap<String, BTreeMap<String, f64>>,
    memory_total: Option<u64>,
    memory_available: Option<u64>,
    disk_size: Option<u64>,
    disk_free: Option<u64>,
    disk_available: Option<u64>,
    network: BTreeMap<String, (Option<u64>, Option<u64>)>,
}

const NAMES: &[&str] = &[
    "incus_cpu_seconds_total",
    "incus_memory_MemTotal_bytes",
    "incus_memory_MemAvailable_bytes",
    "incus_filesystem_size_bytes",
    "incus_filesystem_free_bytes",
    "incus_filesystem_avail_bytes",
    "incus_network_receive_bytes_total",
    "incus_network_transmit_bytes_total",
];

pub fn parse(text: &str) -> Result<BTreeMap<Uuid, RawVmMetrics>> {
    ensure!(
        text.len() <= MAX_METRICS_BYTES,
        "Incus metrics response exceeds limit"
    );
    let mut pending: BTreeMap<Uuid, PendingVm> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name_end = line.find(['{', ' ', '\t']).unwrap_or(line.len());
        let name = &line[..name_end];
        if !NAMES.contains(&name) {
            continue;
        }
        if line.len() > 16384 {
            continue;
        }
        let Ok((labels, tail)) = labels(&line[name_end..]) else {
            continue;
        };
        if labels.get("type").map(String::as_str) != Some("virtual-machine")
            || labels.get("project").map(String::as_str) != Some("default")
        {
            continue;
        }
        let Some(instance) = labels.get("name") else {
            continue;
        };
        let Some(id_text) = instance
            .strip_prefix("hc-")
            .filter(|value| value.len() == 32)
        else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(id_text) else {
            continue;
        };
        if instance != &format!("hc-{}", id.simple()) {
            continue;
        }
        let Ok(value) = metric_value(tail) else {
            continue;
        };
        let vm = pending.entry(id).or_default();
        if record_metric(vm, name, &labels, value).is_err() {
            vm.invalid = true;
        }
    }
    let mut output = BTreeMap::new();
    for (id, vm) in pending {
        if vm.invalid {
            continue;
        }
        let root_filesystem = match (vm.disk_size, vm.disk_free, vm.disk_available) {
            (Some(total), Some(free), Some(available))
                if total > 0 && available <= free && free <= total =>
            {
                VmFilesystemUsage {
                    total_bytes: total,
                    free_bytes: free,
                    available_bytes: available,
                }
            }
            _ => continue,
        };
        let cpu = vm
            .cpu
            .into_iter()
            .filter(|(_, modes)| {
                modes.contains_key("idle")
                    && modes.contains_key("user")
                    && modes.contains_key("system")
            })
            .collect();
        let memory = match (vm.memory_total, vm.memory_available) {
            (Some(total), Some(available)) if total > 0 && available <= total => {
                Some(VmMemoryUsage {
                    total_bytes: total,
                    available_bytes: available,
                })
            }
            _ => None,
        };
        let network = vm
            .network
            .into_iter()
            .filter_map(|(interface, counters)| Some((interface, (counters.0?, counters.1?))))
            .collect();
        output.insert(
            id,
            RawVmMetrics {
                cpu,
                memory,
                root_filesystem: Some(root_filesystem),
                network,
            },
        );
    }
    Ok(output)
}

fn metric_value(tail: &str) -> Result<f64> {
    ensure!(
        tail.starts_with([' ', '\t']),
        "Missing Incus metric value separator"
    );
    let mut fields = tail.split_whitespace();
    let value: f64 = fields
        .next()
        .context("Missing Incus metric value")?
        .parse()?;
    if let Some(timestamp) = fields.next() {
        timestamp
            .parse::<i64>()
            .context("Invalid Incus metric timestamp")?;
    }
    ensure!(fields.next().is_none(), "Unexpected Incus metric fields");
    Ok(value)
}

fn record_metric(
    vm: &mut PendingVm,
    name: &str,
    labels: &BTreeMap<String, String>,
    value: f64,
) -> Result<()> {
    ensure!(
        value.is_finite() && value >= 0.0,
        "Invalid Incus metric value"
    );
    match name {
        "incus_cpu_seconds_total" => {
            let Some(cpu) = labels.get("cpu").filter(|cpu| cpu.len() <= 32) else {
                return Ok(());
            };
            let Some(mode) = labels.get("mode").filter(|mode| {
                matches!(
                    mode.as_str(),
                    "user" | "nice" | "system" | "idle" | "iowait" | "irq" | "softirq" | "steal"
                )
            }) else {
                return Ok(());
            };
            ensure!(
                vm.cpu
                    .entry(cpu.clone())
                    .or_default()
                    .insert(mode.clone(), value)
                    .is_none(),
                "Duplicate Incus CPU metric"
            );
        }
        "incus_memory_MemTotal_bytes" => set_once(&mut vm.memory_total, bytes(value)?)?,
        "incus_memory_MemAvailable_bytes" => set_once(&mut vm.memory_available, bytes(value)?)?,
        "incus_filesystem_size_bytes"
        | "incus_filesystem_free_bytes"
        | "incus_filesystem_avail_bytes" => {
            if labels.get("mountpoint").map(String::as_str) != Some("/") {
                return Ok(());
            }
            let target = match name {
                "incus_filesystem_size_bytes" => &mut vm.disk_size,
                "incus_filesystem_free_bytes" => &mut vm.disk_free,
                _ => &mut vm.disk_available,
            };
            set_once(target, bytes(value)?)?;
        }
        _ => {
            let Some(interface) = labels.get("device").filter(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && !name.chars().any(char::is_control)
                    && name.as_str() != "lo"
            }) else {
                return Ok(());
            };
            if vm.network.len() >= 64 && !vm.network.contains_key(interface) {
                return Ok(());
            }
            let counters = vm.network.entry(interface.clone()).or_default();
            if name == "incus_network_receive_bytes_total" {
                set_once(&mut counters.0, bytes(value)?)?;
            } else {
                set_once(&mut counters.1, bytes(value)?)?;
            }
        }
    }
    Ok(())
}

fn set_once(target: &mut Option<u64>, value: u64) -> Result<()> {
    ensure!(target.replace(value).is_none(), "Ambiguous Incus metric");
    Ok(())
}

fn bytes(value: f64) -> Result<u64> {
    ensure!(
        value.fract() == 0.0 && value < u64::MAX as f64,
        "Invalid Incus byte counter"
    );
    Ok(value as u64)
}

fn labels(mut input: &str) -> Result<(BTreeMap<String, String>, &str)> {
    let mut output = BTreeMap::new();
    input = input
        .strip_prefix('{')
        .context("Missing Incus metric labels")?;
    loop {
        input = input.trim_start();
        if let Some(tail) = input.strip_prefix('}') {
            return Ok((output, tail));
        }
        let key_end = input.find('=').context("Invalid Incus metric label")?;
        let key = input[..key_end].trim();
        ensure!(
            !key.is_empty()
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "Invalid Incus metric label key"
        );
        input = input[key_end + 1..]
            .trim_start()
            .strip_prefix('"')
            .context("Unquoted Incus metric label")?;
        let mut value = String::new();
        let mut escaped = false;
        let mut end = None;
        for (offset, character) in input.char_indices() {
            if escaped {
                value.push(match character {
                    '\\' => '\\',
                    '"' => '"',
                    'n' => '\n',
                    _ => bail!("Invalid Incus metric escape"),
                });
                escaped = false;
            } else {
                match character {
                    '\\' => escaped = true,
                    '"' => {
                        end = Some(offset + 1);
                        break;
                    }
                    _ => value.push(character),
                }
            }
        }
        input = &input[end.context("Unterminated Incus metric label")?..];
        ensure!(
            output.len() < 32 && output.insert(key.to_owned(), value).is_none(),
            "Duplicate or excessive Incus metric labels"
        );
        input = input.trim_start();
        if !input.starts_with('}') {
            input = input
                .strip_prefix(',')
                .context("Missing Incus metric label separator")?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{ServerConfig, server::WebPkiClientVerifier};
    use std::{path::Path, process::Command};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    const ID: &str = "123456781234123412341234567890ab";
    const SERVER_NAME: &str = "metrics.test";

    struct TestIdentity {
        certificate: PathBuf,
        key: PathBuf,
        combined: PathBuf,
    }

    struct TestPki {
        directory: tempfile::TempDir,
        server: TestIdentity,
        client: TestIdentity,
    }

    impl TestIdentity {
        fn generate(directory: &Path, label: &str, purpose: &str) -> Self {
            let certificate = directory.join(format!("{label}.crt"));
            let key = directory.join(format!("{label}.key"));
            let combined = directory.join(format!("{label}.pem"));
            let config = directory.join(format!("{label}.cnf"));
            let subject = if purpose == "serverAuth" {
                SERVER_NAME
            } else {
                "metrics-client"
            };
            std::fs::write(&config, format!(
                "[req]\nprompt = no\ndistinguished_name = subject\nx509_extensions = extensions\n\
                 [subject]\nCN = {subject}\n\
                 [extensions]\nbasicConstraints = critical,CA:FALSE\n\
                 keyUsage = critical,digitalSignature\nextendedKeyUsage = {purpose}\n\
                 subjectAltName = DNS:{SERVER_NAME}\n"
            )).unwrap();
            let output = Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "1",
                    "-config",
                ])
                .arg(config)
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&certificate)
                .output()
                .expect("openssl is needed to generate loopback mTLS test certificates");
            assert!(
                output.status.success(),
                "openssl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut identity = std::fs::read(&certificate).unwrap();
            identity.extend_from_slice(&std::fs::read(&key).unwrap());
            std::fs::write(&combined, identity).unwrap();
            Self {
                certificate,
                key,
                combined,
            }
        }

        fn certificates(&self) -> Vec<CertificateDer<'static>> {
            CertificateDer::pem_slice_iter(&std::fs::read(&self.certificate).unwrap())
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        }
    }

    impl TestPki {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let server = TestIdentity::generate(directory.path(), "server", "serverAuth");
            let client = TestIdentity::generate(directory.path(), "client", "clientAuth");
            Self {
                directory,
                server,
                client,
            }
        }
    }

    struct CapturedRequest {
        certificates: Vec<CertificateDer<'static>>,
        server_name: Option<String>,
        headers: String,
    }

    async fn tls_server(
        pki: &TestPki,
        response: Vec<u8>,
    ) -> (MetricsConfig, JoinHandle<Result<CapturedRequest>>) {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        for certificate in pki.client.certificates() {
            roots.add(certificate).unwrap();
        }
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .unwrap();
        let server = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                pki.server.certificates(),
                PrivateKeyDer::from_pem_slice(&std::fs::read(&pki.server.key).unwrap()).unwrap(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = MetricsConfig {
            port,
            server_name: SERVER_NAME.into(),
            client_identity: pki.client.combined.clone(),
            server_certificate: pki.server.certificate.clone(),
        };
        let task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let (socket, _) = listener.accept().await?;
                let mut stream = acceptor
                    .accept(socket)
                    .await
                    .context("Accepting metrics TLS")?;
                let certificates = stream
                    .get_ref()
                    .1
                    .peer_certificates()
                    .context("Missing client certificate")?
                    .to_vec();
                let server_name = stream.get_ref().1.server_name().map(str::to_owned);
                let mut headers = Vec::new();
                while !headers.windows(4).any(|part| part == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = stream.read(&mut chunk).await?;
                    ensure!(
                        count > 0 && headers.len() + count <= 16384,
                        "Invalid metrics HTTP request"
                    );
                    headers.extend_from_slice(&chunk[..count]);
                }
                stream.write_all(&response).await?;
                stream.shutdown().await?;
                Ok(CapturedRequest {
                    certificates,
                    server_name,
                    headers: String::from_utf8(headers)?,
                })
            })
            .await
            .context("Loopback metrics server timed out")?
        });
        (config, task)
    }

    fn http_response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn sample(name: &str, extra: &str, value: &str) -> String {
        let prefix = if extra.is_empty() {
            String::new()
        } else {
            format!("{},", extra.trim_start_matches(','))
        };
        format!(
            "{name}{{{prefix}name=\"hc-{ID}\",project=\"default\",type=\"virtual-machine\"}} {value}\n"
        )
    }

    pub(super) fn fixture() -> String {
        let mut text = String::new();
        for (name, extra, value) in [
            ("incus_memory_MemTotal_bytes", "", "1000"),
            ("incus_memory_MemAvailable_bytes", "", "600"),
            (
                "incus_filesystem_size_bytes",
                ",mountpoint=\"/\",device=\"/dev/sda2\"",
                "10000",
            ),
            (
                "incus_filesystem_free_bytes",
                ",mountpoint=\"/\",device=\"/dev/sda2\"",
                "4000",
            ),
            (
                "incus_filesystem_avail_bytes",
                ",mountpoint=\"/\",device=\"/dev/sda2\"",
                "3500",
            ),
            ("incus_cpu_seconds_total", ",cpu=\"0\",mode=\"idle\"", "70"),
            ("incus_cpu_seconds_total", ",cpu=\"0\",mode=\"user\"", "20"),
            (
                "incus_cpu_seconds_total",
                ",cpu=\"0\",mode=\"system\"",
                "10",
            ),
            (
                "incus_network_receive_bytes_total",
                ",device=\"enp5s0\"",
                "1000",
            ),
            (
                "incus_network_transmit_bytes_total",
                ",device=\"enp5s0\"",
                "2000",
            ),
        ] {
            text.push_str(&sample(name, extra, value));
        }
        text
    }

    #[test]
    fn guest_filesystem_memory_and_interface_counters_are_distinct() {
        let mut text = fixture();
        text.push_str(&sample(
            "incus_filesystem_size_bytes",
            ",mountpoint=\"/run\"",
            "999999",
        ));
        text.push_str(&sample(
            "incus_network_receive_bytes_total",
            ",device=\"lo\"",
            "999999",
        ));
        let parsed = parse(&text).unwrap();
        let vm = &parsed[&Uuid::parse_str(ID).unwrap()];
        assert_eq!(vm.memory.as_ref().unwrap().used_bytes(), 400);
        let disk = vm.root_filesystem.as_ref().unwrap();
        assert_eq!(disk.used_bytes(), 6000);
        assert_eq!(disk.available_bytes, 3500);
        assert_eq!(vm.network.len(), 1);
        assert_eq!(vm.network["enp5s0"], (1000, 2000));
        assert_eq!(vm.cpu.len(), 1);
    }

    #[test]
    fn isolates_duplicates_bad_labels_nonfinite_negative_and_fractional_bytes() {
        assert!(parse(&(fixture() + &fixture())).unwrap().is_empty());
        for value in ["NaN", "+Inf", "-1", "1.5", "18446744073709551616"] {
            assert!(
                parse(&sample("incus_memory_MemTotal_bytes", "", value))
                    .unwrap()
                    .is_empty()
            );
        }
        for extra in [
            ",project=\"other\"",
            ",device=unquoted",
            ",device=\"bad\\tvalue\"",
            ",device=\"unterminated",
        ] {
            assert!(
                parse(&sample("incus_memory_MemTotal_bytes", extra, "1"))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn rejects_invalid_gauges_and_never_uses_qemu_fallback_memory_as_guest_usage() {
        let mut fallback = sample("incus_memory_MemTotal_bytes", "", "1000")
            + &sample("incus_memory_MemAvailable_bytes", "", "600")
            + &sample(
                "incus_network_receive_bytes_total",
                ",device=\"tap0\"",
                "10",
            );
        for mode in [
            "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal",
        ] {
            fallback.push_str(&sample(
                "incus_cpu_seconds_total",
                &format!(",cpu=\"0\",mode=\"{mode}\""),
                if mode == "user" { "20" } else { "0" },
            ));
        }
        assert!(parse(&fallback).unwrap().is_empty());
        let invalid = fixture().replace("} 600\n", "} 1001\n");
        let parsed = parse(&invalid).unwrap();
        let vm = &parsed[&Uuid::parse_str(ID).unwrap()];
        assert!(vm.memory.is_none());
        assert!(vm.root_filesystem.is_some());
        assert!(
            parse(&fixture().replace("} 3500\n", "} 4500\n"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_guest_labels_and_duplicates_cannot_poison_other_vms() {
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let other_id = Uuid::parse_str(other).unwrap();
        let healthy = fixture().replace(ID, other);
        let malformed = [
            sample("incus_network_receive_bytes_total", ",device=\"a\"b\"", "1"),
            sample("incus_network_receive_bytes_total", ",device=\"a\\b\"", "1"),
            sample(
                "incus_network_receive_bytes_total",
                ",device=\"unterminated",
                "1",
            ),
            sample("incus_memory_MemTotal_bytes", "", "NaN"),
            sample(
                "incus_filesystem_size_bytes",
                ",mountpoint=\"/\",device=\"bind\"",
                "10000",
            ),
        ];
        for broken in malformed {
            for untrusted in [
                broken.clone(),
                broken.replace("virtual-machine", "container"),
                broken.replace("default", "other"),
            ] {
                let parsed = parse(&(healthy.clone() + &fixture() + &untrusted)).unwrap();
                assert!(parsed.contains_key(&other_id));
                assert_eq!(parsed[&other_id].memory.as_ref().unwrap().used_bytes(), 400);
            }
        }
        let parsed = parse(&(healthy + &fixture() + &fixture())).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key(&other_id));
    }

    #[test]
    fn container_heavy_guests_keep_bounded_interfaces_without_losing_other_samples() {
        let mut text = fixture();
        for index in 0..100 {
            let device = format!(",device=\"veth{index}\"");
            text.push_str(&sample("incus_network_receive_bytes_total", &device, "10"));
            text.push_str(&sample("incus_network_transmit_bytes_total", &device, "20"));
        }
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        text.push_str(&fixture().replace(ID, other));
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[&Uuid::parse_str(ID).unwrap()].network.len(), 64);
        assert_eq!(parsed[&Uuid::parse_str(other).unwrap()].network.len(), 1);
    }

    #[test]
    fn injected_identity_before_a_malformed_tail_cannot_hide_another_vm() {
        let other = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let healthy = fixture() + &fixture().replace(ID, other);
        let injected = format!(
            "incus_filesystem_size_bytes{{device=\"/dev/sda2\",fstype=\"ext4\",mountpoint=\"/\",name=\"hc-{other}\",project=\"default\",type=\"virtual-machine\"}}\",name=\"hc-{ID}\",project=\"default\",type=\"virtual-machine\"}} 10000\n"
        );
        for malformed in [
            injected,
            sample("incus_memory_MemTotal_bytes", "", "not-a-number").replace(ID, other),
            sample("incus_memory_MemTotal_bytes", "", "1000 invalid-timestamp").replace(ID, other),
            sample("incus_memory_MemTotal_bytes", "", "1000 123 extra").replace(ID, other),
        ] {
            let parsed = parse(&(healthy.clone() + &malformed)).unwrap();
            assert_eq!(parsed.len(), 2);
            assert_eq!(
                parsed[&Uuid::parse_str(other).unwrap()]
                    .memory
                    .as_ref()
                    .unwrap()
                    .used_bytes(),
                400
            );
        }
    }

    #[test]
    fn ignores_containers_other_projects_and_unmanaged_names() {
        for text in [
            fixture().replace("virtual-machine", "container"),
            fixture().replace("default", "other"),
            fixture().replace("hc-", "other-"),
        ] {
            assert!(parse(&text).unwrap().is_empty());
        }
    }

    #[test]
    fn handles_escaped_labels_and_scientific_notation() {
        let text = fixture()
            .replace("/dev/sda2", "a\\\"b\\\\c")
            .replace("} 1000\n", "} 1e3\n");
        assert_eq!(parse(&text).unwrap().len(), 1);
    }

    #[test]
    fn metrics_config_rejects_addresses_urls_and_zero_port_before_reading_secrets() {
        let mut config = MetricsConfig {
            port: 8444,
            server_name: "localhost".into(),
            client_identity: PathBuf::new(),
            server_certificate: PathBuf::new(),
        };
        for name in [
            "127.0.0.1",
            "https://host",
            "host:8444",
            "user@host",
            "host/path",
            "",
            "0x08080808",
            "134744072",
            "0177.0.0.1",
            "127.1",
            "0x7f000001",
            "2130706433",
        ] {
            config.server_name = name.into();
            assert!(
                MetricsClient::new(&config)
                    .unwrap_err()
                    .to_string()
                    .contains("DNS name")
            );
        }
        config.server_name = "localhost".into();
        config.port = 0;
        assert!(
            MetricsClient::new(&config)
                .unwrap_err()
                .to_string()
                .contains("port")
        );
    }

    #[tokio::test]
    async fn loopback_mtls_accepts_only_the_configured_server_and_sends_configured_identity() {
        let pki = TestPki::new();
        let (config, server) =
            tls_server(&pki, http_response("200 OK", "", fixture().as_bytes())).await;
        let metrics = MetricsClient::new(&config).unwrap().fetch().await.unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[&Uuid::parse_str(ID).unwrap()]
                .memory
                .as_ref()
                .unwrap()
                .used_bytes(),
            400
        );
        let request = server.await.unwrap().unwrap();
        assert_eq!(request.certificates, pki.client.certificates());
        assert_eq!(request.server_name.as_deref(), Some(SERVER_NAME));
        assert!(request.headers.starts_with("GET /1.0/metrics HTTP/1.1\r\n"));
        assert!(
            request
                .headers
                .to_ascii_lowercase()
                .contains(&format!("\r\nhost: {SERVER_NAME}:{}\r\n", config.port))
        );
    }

    #[tokio::test]
    async fn loopback_mtls_rejects_wrong_server_certificate_or_dns_name() {
        let pki = TestPki::new();
        let other = TestIdentity::generate(pki.directory.path(), "other-server", "serverAuth");
        for wrong_name in [false, true] {
            let (mut config, server) =
                tls_server(&pki, http_response("200 OK", "", fixture().as_bytes())).await;
            if wrong_name {
                config.server_name = "wrong.metrics.test".into();
            } else {
                config.server_certificate = other.certificate.clone();
            }
            let error = MetricsClient::new(&config)
                .unwrap()
                .fetch()
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}")
                    .to_ascii_lowercase()
                    .contains("certificate"),
                "{error:#}"
            );
            assert!(server.await.unwrap().is_err());
        }
    }

    #[tokio::test]
    async fn loopback_mtls_rejects_an_untrusted_client_certificate() {
        let pki = TestPki::new();
        let other = TestIdentity::generate(pki.directory.path(), "other-client", "clientAuth");
        let (mut config, server) =
            tls_server(&pki, http_response("200 OK", "", fixture().as_bytes())).await;
        config.client_identity = other.combined;
        assert!(MetricsClient::new(&config).unwrap().fetch().await.is_err());
        let error = server
            .await
            .unwrap()
            .err()
            .expect("Untrusted client must fail server authentication");
        assert!(
            format!("{error:#}")
                .to_ascii_lowercase()
                .contains("certificate"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn loopback_mtls_requires_a_client_certificate() {
        let pki = TestPki::new();
        let (config, server) =
            tls_server(&pki, http_response("200 OK", "", fixture().as_bytes())).await;
        let mut roots = RootCertStore::empty();
        for certificate in pki.server.certificates() {
            roots.add(certificate).unwrap();
        }
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(tls));
        let result: Result<()> = async {
            let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, config.port)).await?;
            let mut stream = connector
                .connect(SERVER_NAME.try_into().unwrap(), socket)
                .await?;
            stream
                .write_all(
                    b"GET /1.0/metrics HTTP/1.1\r\nHost: metrics.test\r\nConnection: close\r\n\r\n",
                )
                .await?;
            stream.read_to_end(&mut Vec::new()).await?;
            Ok(())
        }
        .await;
        assert!(result.is_err());
        let error = server
            .await
            .unwrap()
            .err()
            .expect("Missing client identity must fail server authentication");
        assert!(
            format!("{error:#}")
                .to_ascii_lowercase()
                .contains("certificate"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn loopback_collector_rejects_redirects_without_contacting_the_target() {
        let pki = TestPki::new();
        let target = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        target.set_nonblocking(true).unwrap();
        let location = format!(
            "Location: https://127.0.0.1:{}/forbidden\r\n",
            target.local_addr().unwrap().port()
        );
        for status in [
            "301 Moved Permanently",
            "302 Found",
            "303 See Other",
            "307 Temporary Redirect",
            "308 Permanent Redirect",
        ] {
            let (config, server) =
                tls_server(&pki, http_response(status, &location, fixture().as_bytes())).await;
            let error = MetricsClient::new(&config)
                .unwrap()
                .fetch()
                .await
                .unwrap_err();
            assert!(error.to_string().contains("HTTP status"), "{error:#}");
            server.await.unwrap().unwrap();
            assert_eq!(
                target.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[tokio::test]
    async fn loopback_collector_rejects_oversized_content_length_before_reading_body() {
        let pki = TestPki::new();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_METRICS_BYTES + 1
        )
        .into_bytes();
        let (config, server) = tls_server(&pki, response).await;
        let error = MetricsClient::new(&config)
            .unwrap()
            .fetch()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds limit"), "{error:#}");
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loopback_collector_enforces_the_streaming_body_limit_without_content_length() {
        let pki = TestPki::new();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            MAX_METRICS_BYTES + 1
        )
        .into_bytes();
        response.resize(response.len() + MAX_METRICS_BYTES + 1, b' ');
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let (config, server) = tls_server(&pki, response).await;
        let error = MetricsClient::new(&config)
            .unwrap()
            .fetch()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds limit"), "{error:#}");
        let _ = server.await.unwrap();
    }

    #[tokio::test]
    async fn loopback_collector_accepts_body_exactly_at_the_limit() {
        let pki = TestPki::new();
        let mut body = fixture().into_bytes();
        body.push(b'#');
        body.resize(MAX_METRICS_BYTES, b' ');
        let (config, server) = tls_server(&pki, http_response("200 OK", "", &body)).await;
        assert_eq!(
            MetricsClient::new(&config)
                .unwrap()
                .fetch()
                .await
                .unwrap()
                .len(),
            1
        );
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loopback_collector_ignores_non_utf8_lines_without_losing_healthy_samples() {
        let pki = TestPki::new();
        let mut body = fixture().into_bytes();
        body.extend_from_slice(b"incus_network_receive_bytes_total{device=\"\xff\"} 1\n");
        let (config, server) = tls_server(&pki, http_response("200 OK", "", &body)).await;
        assert_eq!(
            MetricsClient::new(&config)
                .unwrap()
                .fetch()
                .await
                .unwrap()
                .len(),
            1
        );
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn loopback_collector_rejects_http_errors_and_truncated_bodies() {
        let pki = TestPki::new();
        let mut truncated = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            fixture().len() + 1
        )
        .into_bytes();
        truncated.extend_from_slice(fixture().as_bytes());
        for response in [
            http_response("500 Internal Server Error", "", fixture().as_bytes()),
            truncated,
        ] {
            let (config, server) = tls_server(&pki, response).await;
            assert!(MetricsClient::new(&config).unwrap().fetch().await.is_err());
            server.await.unwrap().unwrap();
        }
    }
}
