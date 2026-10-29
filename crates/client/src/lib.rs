use anyhow::{Context, Result, ensure};
use homecloud_core::*;
use iroh::{
    Endpoint, EndpointAddr, SecretKey,
    endpoint::{Connection, presets},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

mod profile;
pub use profile::*;

pub fn default_profile() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
        .join(".config/homecloud/client.json")
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        // Only remove the unique temporary created by this invocation.
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(())
}
pub fn load_secret(path: &Path) -> Result<SecretKey> {
    if !path.exists() {
        let key = SecretKey::generate();
        private_write(path, &key.to_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(path)?.permissions().mode() & 0o077 == 0,
            "Endpoint secret must be private"
        );
    }
    let bytes: [u8; 32] = std::fs::read(path)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid endpoint secret length"))?;
    Ok(SecretKey::from_bytes(&bytes))
}
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &impl Serialize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "Frame too large");
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
pub async fn read_frame<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T> {
    let length = reader.read_u32().await? as usize;
    ensure!(length <= MAX_FRAME, "Frame too large");
    let mut data = vec![0; length];
    reader.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}

/// Possession of the enrolled device key, bound to this QUIC/TLS connection.
/// A process uses its own transport endpoint; GUI and SSH helpers can coexist
/// without replacing one another's registration on an Iroh relay.
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceProof {
    pub device: iroh::PublicKey,
    pub signature: iroh::Signature,
}
impl DeviceProof {
    fn message(connection: &Connection) -> Result<Vec<u8>> {
        let mut binding = [0u8; 32];
        connection
            .export_keying_material(
                &mut binding,
                b"EXPORTER-homecloud-device-auth-v1",
                connection.alpn(),
            )
            .map_err(|_| anyhow::anyhow!("Cannot bind device authentication to TLS"))?;
        let mut message = b"homecloud/device-proof/v1\0".to_vec();
        message.extend_from_slice(&binding);
        Ok(message)
    }
    pub fn sign(connection: &Connection, device: &SecretKey) -> Result<Self> {
        Ok(Self {
            device: device.public(),
            signature: device.sign(&Self::message(connection)?),
        })
    }
    pub fn verify(&self, connection: &Connection) -> Result<String> {
        self.device
            .verify(&Self::message(connection)?, &self.signature)
            .context("Invalid device proof")?;
        Ok(self.device.to_string())
    }
}

#[derive(Clone, Copy, Default)]
pub struct ConnectionOptions {
    /// Diagnostic fallback for networks whose UDP paths are unreliable.
    pub relay_only: bool,
}

#[derive(Clone)]
pub struct Client {
    endpoint: Endpoint,
    device: SecretKey,
    server: EndpointAddr,
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn export_ssh(
    profile: &Path,
    vm: &Vm,
    executable: &Path,
    install: bool,
) -> Result<(PathBuf, String)> {
    export_ssh_with_options(
        profile,
        vm,
        executable,
        install,
        ConnectionOptions::default(),
    )
}
pub fn export_ssh_with_options(
    profile: &Path,
    vm: &Vm,
    executable: &Path,
    install: bool,
    options: ConnectionOptions,
) -> Result<(PathBuf, String)> {
    ensure!(
        vm.ssh_host_key.is_some(),
        "VM SSH host key is not enrolled yet"
    );
    let key = vm.ssh_host_key.as_ref().unwrap();
    let fields: Vec<_> = key.split_whitespace().collect();
    ensure!(
        fields.len() >= 2 && fields[0] == "ssh-ed25519",
        "Unexpected guest host-key format"
    );
    let parent = profile
        .parent()
        .context("Profile has no parent directory")?
        .join("ssh");
    std::fs::create_dir_all(&parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let alias = format!("hc-{}", vm.id.simple());
    let trust = format!("homecloud-{}", vm.id);
    let known = parent.join(format!("{}.known_hosts", vm.id));
    let key_line = format!("{trust} {} {}\n", fields[0], fields[1]);
    if known.exists() {
        ensure!(
            std::fs::read_to_string(&known)? == key_line,
            "Guest SSH host key changed; inspect and explicitly remove its saved trust before reenrolling"
        );
    } else {
        private_write(&known, key_line.as_bytes())?;
    }
    let config = parent.join(format!("{}.conf", vm.id));
    let config_quote = |s: &Path| -> Result<String> {
        let s = s.to_str().context("Non-UTF8 path")?;
        ensure!(!s.contains(['\n', '\r']), "Invalid config path");
        Ok(format!(
            "\"{}\"",
            s.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    };
    let contents = format!(
        "Host {alias}\n    HostName {}\n    User dev\n    IdentitiesOnly yes\n    StrictHostKeyChecking yes\n    HostKeyAlias {trust}\n    UserKnownHostsFile {}\n    ProxyCommand {} --profile {} {}ssh-proxy {}\n",
        vm.address(),
        config_quote(&known)?,
        shell_quote(executable.to_str().context("Executable path")?),
        shell_quote(profile.to_str().context("Profile path")?),
        if options.relay_only {
            "--relay-only "
        } else {
            ""
        },
        vm.id
    );
    private_write(&config, contents.as_bytes())?;
    if install {
        let ssh = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".ssh");
        std::fs::create_dir_all(&ssh)?;
        let main = ssh.join("config");
        let mut contents = if main.exists() {
            std::fs::read_to_string(&main)?
        } else {
            String::new()
        };
        let include = format!("Include {}", config_quote(&parent.join("*.conf"))?);
        if !contents.lines().any(|line| line == include) {
            contents = format!("# Homecloud managed VM aliases\n{include}\n\n{contents}");
            private_write(&main, contents.as_bytes())?;
        }
    }
    Ok((config, alias))
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("device", &self.device.public())
            .field("server", &self.server.id)
            .finish()
    }
}
impl Client {
    pub async fn from_profile(profile: Profile) -> Result<Arc<Self>> {
        Self::with_options(profile, ConnectionOptions::default()).await
    }
    pub async fn with_options(profile: Profile, options: ConnectionOptions) -> Result<Arc<Self>> {
        let bytes: [u8; 32] = hex::decode(&profile.device_secret)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid device credential"))?;
        let device = SecretKey::from_bytes(&bytes);
        let builder = Endpoint::builder(presets::N0);
        let builder = if options.relay_only {
            builder.clear_ip_transports()
        } else {
            builder
        };
        let endpoint = builder.bind().await?;
        Ok(Arc::new(Self {
            endpoint,
            device,
            server: profile.server,
        }))
    }
    pub async fn load(path: &Path) -> Result<Arc<Self>> {
        Self::from_profile(Profile::load(path)?).await
    }
    pub async fn enroll(
        path: &Path,
        server: EndpointAddr,
        token: String,
    ) -> Result<(Arc<Self>, User)> {
        Self::enroll_with_options(path, server, token, ConnectionOptions::default()).await
    }
    pub async fn enroll_with_options(
        path: &Path,
        server: EndpointAddr,
        token: String,
        options: ConnectionOptions,
    ) -> Result<(Arc<Self>, User)> {
        Self::enroll_inner(path, server, token, options, None).await
    }
    /// Inject storage for deterministic tests or another native platform store.
    pub async fn enroll_with_store(
        path: &Path,
        server: EndpointAddr,
        token: String,
        options: ConnectionOptions,
        store: &dyn CredentialStore,
    ) -> Result<(Arc<Self>, User)> {
        Self::enroll_inner(path, server, token, options, Some(store)).await
    }
    async fn enroll_inner(
        path: &Path,
        server: EndpointAddr,
        token: String,
        options: ConnectionOptions,
        store: Option<&dyn CredentialStore>,
    ) -> Result<(Arc<Self>, User)> {
        let profile = if path.exists() {
            let old = if let Some(store) = store {
                Profile::load_with_store(path, store)?
            } else {
                Profile::load(path)?
            };
            ensure!(
                old.server.id == server.id,
                "Profile already pins another host; choose another profile path"
            );
            Profile { server, ..old }
        } else {
            Profile {
                server,
                device_secret: hex::encode(SecretKey::generate().to_bytes()),
            }
        };
        // Persist possession before redeeming a single-use token. Lost replies
        // can reconnect with the same device instead of stranding enrollment.
        if let Some(store) = store {
            profile.save_with_store(path, store)?;
        } else {
            profile.save(path)?;
        }
        let client = Self::with_options(profile, options).await?;
        match client.rpc(Request::Enroll { token }).await? {
            Response::Enrolled(user) => Ok((client, user)),
            other => anyhow::bail!("Unexpected enrollment response: {other:?}"),
        }
    }
    pub fn device_id(&self) -> String {
        self.device.public().to_string()
    }
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
    pub fn transport_id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
    pub fn server_id(&self) -> String {
        self.server.id.to_string()
    }
    pub async fn rpc(&self, request: Request) -> Result<Response> {
        match self.rpc_response(request).await? {
            Response::Error { message } | Response::CreateRejected { message, .. } => {
                anyhow::bail!("{message}")
            }
            response => Ok(response),
        }
    }
    /// Keep an authenticated server rejection distinct from transport uncertainty.
    pub async fn rpc_response(&self, request: Request) -> Result<Response> {
        tokio::time::timeout(Duration::from_secs(45), async {
            let conn = self
                .endpoint
                .connect(self.server.clone(), CONTROL_ALPN)
                .await?;
            tracing::debug!(transport=%self.endpoint.id(), paths=?conn.paths(), "Control connection established");
            let (mut send, mut recv) = conn.open_bi().await?;
            write_frame(&mut send, &DeviceProof::sign(&conn, &self.device)?).await?;
            write_frame(&mut send, &request).await?;
            send.finish()?;
            let response: Response = read_frame(&mut recv).await.inspect_err(|error| {
                tracing::debug!(%error, paths=?conn.paths(), close_reason=?conn.close_reason(), "Control response failed");
            })?;
            conn.close(0u32.into(), b"complete");
            Ok(response)
        })
        .await
        .context("Host request timed out")?
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        match self.rpc(Request::Snapshot).await? {
            Response::Snapshot(snapshot) => Ok(*snapshot),
            _ => anyhow::bail!("Unexpected host response"),
        }
    }
    /// Binary-clean OpenSSH ProxyCommand: protocol metadata is consumed here,
    /// and only guest SSH bytes reach stdout.
    pub async fn ssh_stdio(&self, vm_id: uuid::Uuid) -> Result<()> {
        let (conn, mut send, mut recv) = tokio::time::timeout(Duration::from_secs(45), async {
            let conn = self.endpoint.connect(self.server.clone(), SSH_ALPN).await?;
            tracing::debug!(transport=%self.endpoint.id(), paths=?conn.paths(), "SSH connection established");
            let (mut send, mut recv) = conn.open_bi().await?;
            write_frame(&mut send, &DeviceProof::sign(&conn, &self.device)?).await?;
            write_frame(&mut send, &Request::Ssh { vm_id }).await?;
            match read_frame::<_, Response>(&mut recv).await? {
                Response::SshReady => {}
                Response::Error { message } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Invalid SSH gateway response"),
            }
            anyhow::Ok((conn, send, recv))
        })
        .await
        .context("SSH gateway authorization timed out")??;
        let upload = async {
            tokio::io::copy(&mut tokio::io::stdin(), &mut send).await?;
            send.finish()?;
            anyhow::Ok(())
        };
        let download = async {
            let mut out = tokio::io::stdout();
            tokio::io::copy(&mut recv, &mut out).await?;
            out.flush().await?;
            anyhow::Ok(())
        };
        tokio::try_join!(upload, download)?;
        conn.close(0u32.into(), b"SSH complete");
        self.endpoint.close().await;
        Ok(())
    }
}
