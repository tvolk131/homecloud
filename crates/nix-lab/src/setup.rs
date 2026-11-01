use super::*;
const VERSION: &str = "2.35.4";

/// All mutable fixture selection comes from the private manifest.
impl Runner {
    pub(super) async fn root_step(&self, script: &str) -> Result<String> {
        self.verify().await?;
        let out = self.remote(&(self.fence()? + script), 1200).await?;
        self.verify().await?;
        Ok(out)
    }
    pub async fn install(&self, phase: &str) -> Result<()> {
        let s = self.state.as_ref().context("Prepare first")?;
        let run = s.run.simple().to_string();
        let base = format!("/var/lib/homecloud/nix-lab/{run}");
        let fixture = |role: &str| -> Result<String> {
            let f = s
                .fixtures
                .iter()
                .find(|f| f.role == role)
                .context("Fixture role")?;
            Ok(format!("hc-{}", f.id.context("Fixture ID")?.simple()))
        };
        let builder = fixture("builder")?;
        let a = fixture("client-a")?;
        let b = fixture("client-b")?;
        let installer = installer(&run);
        let store_gib = s.store_gib;
        let script = match phase {
            "host" => format!(
                r#"
base={base}
install -d -m 0700 "$base"
if ! zfs list hclab/nix-lab-{run} >/dev/null 2>&1; then
    test "$(uname -m)" = x86_64
    # A fresh run must not adopt an existing Nix installation or lab identity.
    for path in /nix /etc/nix /etc/profile.d/nix.sh /etc/bash.bashrc.backup-before-nix /etc/systemd/system/nix-daemon.service /etc/systemd/system/nix-daemon.socket /root/.nix-profile /root/.nix-defexpr; do
        test ! -e "$path"; test ! -L "$path"
    done
    for identity in nixbld hcnix-no-builders; do
        if getent group "$identity" >/dev/null; then exit 1; fi
    done
    for identity in hcnix-a hcnix-b $(seq -f 'nixbld%g' 1 32); do
        if getent passwd "$identity" >/dev/null; then exit 1; fi
    done
    zfs create -o quota={store_gib}G -o reservation={store_gib}G -o compression=zstd -o acltype=posix -o mountpoint=/nix -o org.homecloud:nix-lab={run} hclab/nix-lab-{run}
fi
test "$(zfs get -H -o value org.homecloud:nix-lab hclab/nix-lab-{run})" = {run}
zfs set acltype=posix hclab/nix-lab-{run}
touch "$base/active"
{installer}
/nix/var/nix/profiles/default/bin/nix --version
zfs list -Hp -o name,used,available,quota,reservation hclab/nix-lab-{run}
"#
            ),
            "builder" => {
                let inside = format!(
                    r#"
{installer}
if ! id hcnix-build >/dev/null 2>&1; then useradd --create-home --shell /bin/sh hcnix-build; fi
printf '%s\n' '{run}' > /etc/hcnix-builder-marker
cat > /etc/nix/nix.conf <<'CONF'
experimental-features = nix-command flakes
sandbox = true
sandbox-fallback = false
max-jobs = 1
cores = 1
extra-sandbox-paths = /hc-builder-marker=/etc/hcnix-builder-marker
trusted-users = root hcnix-build
CONF
systemctl restart nix-daemon
/nix/var/nix/profiles/default/bin/nix --version
"#
                );
                format!(
                    "incus exec {} -- sh -se -c {}\n",
                    quote(builder),
                    quote(inside)
                )
            }
            "kernels" => {
                let mut out = String::new();
                for name in [a, b] {
                    let inside = "export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; apt-cache policy linux-generic-hwe-24.04; apt-get install -y -qq --no-install-recommends linux-generic-hwe-24.04 > /var/log/hcnix-kernel-install.log 2>&1 || { tail -60 /var/log/hcnix-kernel-install.log; exit 1; }; ls /boot/vmlinuz-*";
                    out.push_str(&format!(
                        "incus exec {} -- sh -se -c {}\n",
                        quote(&name),
                        quote(inside)
                    ));
                    out.push_str(&format!(r#"
old_boot=$(incus exec {name} -- cat /proc/sys/kernel/random/boot_id)
incus exec {name} -- systemctl reboot || true
attempt=0
until new_boot=$(incus exec {name} -- cat /proc/sys/kernel/random/boot_id) && test "$new_boot" != "$old_boot"; do
    attempt=$((attempt+1)); test "$attempt" -lt 60; sleep 1
done
incus exec {name} -- uname -r
"#));
                }
                out
            }
            // Lab-only workaround for mmap users. The original proof stays uncached.
            // This needs separate coherence/GC review before production use.
            "data-cache" => {
                let mut out = String::new();
                for name in [a, b] {
                    out.push_str(&format!(
                        r#"
    test "$(incus config device get {name} shared-nix source)" = /nix/store
    test "$(incus config device get {name} shared-nix readonly)" = true
    incus config device set {name} shared-nix io.cache=unsafe
"#
                    ));
                }
                out
            }
            "clients" => {
                let mut out = format!("base={}\ntest -f \"$base/active\"\n", quote(&base));
                for (role, name) in [("client-a", a), ("client-b", b)] {
                    // An existing device must match exactly; never silently replace it.
                    out.push_str(&format!(
                        r#"
if incus config device get {name} shared-nix source >/dev/null 2>&1; then
    test "$(incus config device get {name} shared-nix source)" = /nix/store
    test "$(incus config device get {name} shared-nix readonly)" = true
    incus config device set {name} shared-nix io.cache=none
else
    incus config device add {name} shared-nix disk source=/nix/store path=/nix/store readonly=true io.cache=none
fi
"#
                    ));
                    let inside = format!(
                        r#"
install -d -m 0755 /nix/var/nix /etc/nix /usr/local/bin
attempt=0
until findmnt -n -o FSTYPE,OPTIONS /nix/store; do
    attempt=$((attempt+1)); test "$attempt" -lt 30; sleep 1
done
# Incus enforces readonly at the virtiofs server, even when the agent mounts rw.
probe=/nix/store/hc-proof-write-{run}
if touch "$probe" 2>/dev/null; then rm "$probe"; echo 'Shared store unexpectedly writable' >&2; exit 1; fi
mount -o remount,ro /nix/store
nixbin=$(find /nix/store -maxdepth 3 -path '*/bin/nix' -type f | head -1)
test -n "$nixbin"
ln -sf "$nixbin" /usr/local/bin/nix
for tool in nix-store nix-instantiate nix-shell nix-env; do ln -sf "$(dirname "$nixbin")/$tool" "/usr/local/bin/$tool"; done
install -d -o dev -g dev -m 0755 /nix/var/nix/profiles/per-user/dev /nix/var/nix/gcroots/per-user/dev
cat > /etc/nix/nix.conf <<'CONF'
experimental-features = nix-command flakes mounted-ssh-store
CONF
install -d -o dev -g dev -m 0700 /home/dev/.ssh
if ! test -f /home/dev/.ssh/hcnix_ed25519; then runuser -u dev -- ssh-keygen -q -t ed25519 -N '' -f /home/dev/.ssh/hcnix_ed25519; fi
printf '%s\n' '{run}:{role}' > /etc/hcnix-client-marker
/usr/local/bin/nix --version
"#
                    );
                    out.push_str(&format!(
                        "incus exec {} -- sh -se -c {}\n",
                        quote(name),
                        quote(inside)
                    ));
                }
                out
            }
            "roots" => {
                let mut out = format!(
                    r#"
rev=$(jq -r .locked.rev {base}/nixpkgs-metadata.json)
acl_store=$(/nix/var/nix/profiles/default/bin/nix build --no-link --print-out-paths --expr "let pkgs = import (builtins.getFlake \"github:NixOS/nixpkgs/$rev\").outPath {{ system = \"x86_64-linux\"; }}; in pkgs.lib.getBin pkgs.acl")
acl="$acl_store/bin/setfacl"
test -x "$acl"
"#
                );
                for (role, name, user) in [("client-a", a, "hcnix-a"), ("client-b", b, "hcnix-b")] {
                    let state = format!("/nix/lab-roots/{run}/{role}");
                    out.push_str(&format!(r#"
install -d -m 0755 /nix/lab-roots /nix/lab-roots/{run}
install -d -o {user} -g {user} -m 0755 {state} {state}/nix {state}/nix/profiles
uid=$(incus exec {name} -- id -u dev)
for dir in {state} {state}/nix {state}/nix/profiles; do
    "$acl" -m u:"$uid":rwx,u:{user}:rwx,d:u:"$uid":rwx,d:u:{user}:rwx "$dir"
done
if incus config device get {name} shared-nix-state source >/dev/null 2>&1; then
    test "$(incus config device get {name} shared-nix-state source)" = {state}
else
    incus config device add {name} shared-nix-state disk source={state} path={state}
fi
incus exec {name} -- sh -se -c 'attempt=0; until findmnt -n {state}; do attempt=$((attempt+1)); test "$attempt" -lt 30; sleep 1; done'
printf '%s\n' 'export XDG_STATE_HOME={state}' | incus exec {name} -- sh -c 'cat >> /etc/profile.d/hcnix.sh'
"#));
                }
                out
            }
            "transport" => {
                let mut out = format!(
                    r#"
base={base}
test -f "$base/active"
if ! getent group hcnix-no-builders >/dev/null; then groupadd --system hcnix-no-builders; fi
test -z "$(getent group hcnix-no-builders | cut -d: -f4)"
if ! test -f "$base/ssh_host_ed25519_key"; then ssh-keygen -q -t ed25519 -N '' -f "$base/ssh_host_ed25519_key"; fi
if ! test -f "$base/builder_ed25519"; then ssh-keygen -q -t ed25519 -N '' -f "$base/builder_ed25519"; fi
install -d -m 0755 "$base/authorized_keys"
"#
                );
                for (role, name, user) in [("client-a", a, "hcnix-a"), ("client-b", b, "hcnix-b")] {
                    out.push_str(&format!(r#"
if ! id {user} >/dev/null 2>&1; then
    useradd --system --create-home --home-dir /var/lib/homecloud/nix-lab-users/{user} --comment 'Homecloud Nix lab {run}' --shell /bin/sh {user}
fi
test "$(getent passwd {user} | cut -d: -f5)" = 'Homecloud Nix lab {run}'
test "$(getent passwd {user} | cut -d: -f6)" = /var/lib/homecloud/nix-lab-users/{user}
incus exec {name} -- cat /home/dev/.ssh/hcnix_ed25519.pub > "$base/authorized_keys/{user}"
chmod 0644 "$base/authorized_keys/{user}"
install -d -o {user} -g {user} -m 0700 /var/lib/homecloud/nix-lab-users/{user}/.ssh
install -o {user} -g {user} -m 0600 "$base/authorized_keys/{user}" /var/lib/homecloud/nix-lab-users/{user}/.ssh/authorized_keys
ip=$(incus config device get {name} eth0 ipv4.address)
bridge=$(incus config device get {name} eth0 network)
gateway="${{ip%.*}}.1"
printf '%s\n' "$gateway" >> "$base/listen_addresses.tmp"
# This dedicated endpoint accepts only authenticated Nix stdio operations.
if ! nft -a list chain inet homecloud protect_host | grep -q 'hcnix-{run}-{role}'; then
    nft insert rule inet homecloud protect_host iifname "$bridge" ip saddr "$ip" ip daddr "$gateway" tcp dport 12222 accept comment '"hcnix-{run}-{role}"'
fi
pub=$(cat "$base/ssh_host_ed25519_key.pub")
"#));
                    let client = format!(
                        r#"
cat > /home/dev/.ssh/config <<'CONF'
Host hc-nix-store
    HostName @GATEWAY@
    Port 12222
    User {user}
    IdentityFile /home/dev/.ssh/hcnix_ed25519
    UserKnownHostsFile /home/dev/.ssh/hcnix_known_hosts
    StrictHostKeyChecking yes
    IdentitiesOnly yes
CONF
printf '%s\n' '[hc-nix-store]:12222 @PUB@' '[@GATEWAY@]:12222 @PUB@' > /home/dev/.ssh/hcnix_known_hosts
chown dev:dev /home/dev/.ssh/config /home/dev/.ssh/hcnix_known_hosts
chmod 0600 /home/dev/.ssh/config /home/dev/.ssh/hcnix_known_hosts
printf '%s\n' 'export NIX_REMOTE=mounted-ssh-ng://hc-nix-store' > /etc/profile.d/hcnix.sh
"#
                    );
                    // Values here are fixed IPv4/public SSH keys, never private credentials.
                    out.push_str(&format!("client={}\nclient=$(printf '%s' \"$client\" | sed \"s|@GATEWAY@|$gateway|g; s|@PUB@|$pub|g\")\nincus exec {} -- sh -se -c \"$client\"\n",quote(client),quote(name)));
                }
                out.push_str(&format!(r#"
# The host's worker key can run only the Nix worker protocol in the disposable builder.
pub=$(cat "$base/builder_ed25519.pub")
incus exec {builder} -- install -d -o hcnix-build -g hcnix-build -m 0700 /home/hcnix-build/.ssh
printf '%s\n' "restrict,command=\"/nix/var/nix/profiles/default/bin/nix-daemon --stdio\" $pub" | incus exec {builder} -- sh -c 'cat > /home/hcnix-build/.ssh/authorized_keys; chown hcnix-build:hcnix-build /home/hcnix-build/.ssh/authorized_keys; chmod 0600 /home/hcnix-build/.ssh/authorized_keys'
builder_ip=$(incus config device get {builder} eth0 ipv4.address)
builder_pub=$(incus exec {builder} -- cat /etc/ssh/ssh_host_ed25519_key.pub | base64 -w0)
cat > /etc/nix/nix.conf <<CONF
experimental-features = nix-command flakes mounted-ssh-store
sandbox = true
max-jobs = 0
build-users-group = hcnix-no-builders
cores = 1
allowed-users = root hcnix-a hcnix-b
trusted-users = root
builders = ssh-ng://hcnix-build@$builder_ip?base64-ssh-public-host-key=$builder_pub x86_64-linux $base/builder_ed25519 1 1 - -
builders-use-substitutes = true
require-sigs = true
CONF
systemctl restart nix-daemon
cat > "$base/sshd_config" <<CONF
Port 12222
HostKey $base/ssh_host_ed25519_key
PidFile $base/sshd.pid
AuthorizedKeysFile /var/lib/homecloud/nix-lab-users/%u/.ssh/authorized_keys
AllowUsers hcnix-a hcnix-b
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
UsePAM yes
AllowTcpForwarding no
AllowAgentForwarding no
X11Forwarding no
PermitTunnel no
ForceCommand /nix/var/nix/profiles/default/bin/nix-daemon --stdio
LogLevel VERBOSE
CONF
sort -u "$base/listen_addresses.tmp" | sed 's/^/ListenAddress /' >> "$base/sshd_config"
rm "$base/listen_addresses.tmp"
/usr/sbin/sshd -t -f "$base/sshd_config"
cat > /etc/systemd/system/hcnix-lab-sshd.service <<CONF
[Unit]
Description=Disposable Homecloud Nix store endpoint
After=network.target nix-daemon.service
[Service]
ExecStart=/usr/sbin/sshd -D -e -f $base/sshd_config
Restart=on-failure
MemoryMax=256M
[Install]
WantedBy=multi-user.target
CONF
systemctl daemon-reload
systemctl enable --now hcnix-lab-sshd
systemctl restart hcnix-lab-sshd
systemctl is-active nix-daemon hcnix-lab-sshd
"#));
                out
            }
            _ => anyhow::bail!(
                "Unknown phase; choose host, builder, kernels, clients, data-cache, roots, transport"
            ),
        };
        println!("{}", self.root_step(&script).await?);
        Ok(())
    }
}
fn installer(run: &str) -> String {
    format!(
        r#"
if ! test -x /nix/var/nix/profiles/default/bin/nix; then
    stage=$(mktemp -d /var/tmp/hcnix-install.XXXXXX)
    printf '%s\n' '{run}' > "$stage/.homecloud-nix-lab"
    archive=nix-{VERSION}-x86_64-linux.tar.xz
    curl --fail --location --retry 3 --proto '=https' 'https://releases.nixos.org/nix/nix-{VERSION}/nix-{VERSION}-x86_64-linux.tar.xz' -o "$stage/$archive"
    curl --fail --location --retry 3 --proto '=https' 'https://releases.nixos.org/nix/nix-{VERSION}/nix-{VERSION}-x86_64-linux.tar.xz.sha256' -o "$stage/checksum"
    expected=$(cut -d' ' -f1 "$stage/checksum")
    printf '%s  %s\n' "$expected" "$stage/$archive" | sha256sum --check -
    tar -xJf "$stage/$archive" -C "$stage"
    "$stage/nix-{VERSION}-x86_64-linux/install" --daemon --yes --no-channel-add --no-modify-profile > "$stage/install.log" 2>&1 || {{ tail -80 "$stage/install.log"; exit 1; }}
fi
/nix/var/nix/profiles/default/bin/nix --version | grep -F '{VERSION}'
"#
    )
}
