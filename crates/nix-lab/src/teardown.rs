use super::*;

impl Runner {
    /// Remove the owned host installation while fixture identity fencing is still possible.
    pub async fn uninstall(&self) -> Result<()> {
        let s = self.state.as_ref().context("Prepare first")?;
        let run = s.run.simple().to_string();
        let base = format!("/var/lib/homecloud/nix-lab/{run}");
        let dataset = format!("hclab/nix-lab-{run}");
        let mut script = format!(
            r#"
base={base}
test -f "$base/active"
test "$(zfs get -H -o value org.homecloud:nix-lab {dataset})" = {run}
test "$(zfs get -H -o value mountpoint {dataset})" = /nix
test "$(zfs list -H -o name -r {dataset} | wc -l)" -eq 1
test -z "$(zfs list -Ht snapshot -o name -r {dataset})"
# The installer prepended exactly seven lines; reject unrelated shell changes.
tail -n +8 /etc/bash.bashrc | cmp -s - /etc/bash.bashrc.backup-before-nix
test "$(readlink /etc/systemd/system/nix-daemon.service)" = /nix/var/nix/profiles/default/lib/systemd/system/nix-daemon.service
test "$(readlink /etc/systemd/system/nix-daemon.socket)" = /nix/var/nix/profiles/default/lib/systemd/system/nix-daemon.socket
test "$(readlink /etc/tmpfiles.d/nix-daemon.conf)" = /nix/var/nix/profiles/default/lib/tmpfiles.d/nix-daemon.conf
for user in hcnix-a hcnix-b; do
    entry=$(getent passwd "$user")
    test "$(printf '%s' "$entry" | cut -d: -f5)" = 'Homecloud Nix lab {run}'
    test "$(printf '%s' "$entry" | cut -d: -f6)" = "/var/lib/homecloud/nix-lab-users/$user"
done
for number in $(seq 1 32); do
    entry=$(getent passwd "nixbld$number")
    test "$(printf '%s' "$entry" | cut -d: -f3)" -eq "$((30000+number))"
    test "$(printf '%s' "$entry" | cut -d: -f4)" -eq 30000
    test "$(printf '%s' "$entry" | cut -d: -f6)" = /var/empty
done
"#
        );
        for f in s.fixtures.iter().filter(|f| f.role.starts_with("client-")) {
            let name = format!("hc-{}", f.id.context("Fixture ID")?.simple());
            let path = format!("/nix/lab-roots/{run}/{}", f.role);
            script.push_str(&format!(
                r#"
test "$(incus config device get {name} shared-nix source)" = /nix/store
test "$(incus config device get {name} shared-nix readonly)" = true
test "$(incus config device get {name} shared-nix-state source)" = {path}
"#
            ));
        }
        script.push_str(
            r#"
systemctl disable --now hcnix-lab-sshd
systemctl disable --now nix-daemon.socket
systemctl stop nix-daemon.service
"#,
        );
        for f in s.fixtures.iter().filter(|f| f.role.starts_with("client-")) {
            let name = format!("hc-{}", f.id.unwrap().simple());
            let path = format!("/nix/lab-roots/{run}/{}", f.role);
            script.push_str(&format!(
                r#"
incus config device remove {name} shared-nix-state
incus config device remove {name} shared-nix
attempt=0
while incus exec {name} -- mountpoint -q /nix/store || incus exec {name} -- mountpoint -q {path}; do
    attempt=$((attempt+1)); test "$attempt" -lt 30; sleep 1
done
incus exec {name} -- true
"#
            ));
        }
        script.push_str(&format!(r#"
# Remove only this run's narrowly scoped firewall exceptions.
for handle in $(nft -ja list chain inet homecloud protect_host | jq -r '.nftables[] | .rule? // empty | select((.comment // "") | startswith("hcnix-{run}-")) | .handle'); do
    nft delete rule inet homecloud protect_host handle "$handle"
done
cp /etc/nix/nix.conf "$base/final-nix.conf"
rm -f /etc/systemd/system/hcnix-lab-sshd.service /etc/systemd/system/nix-daemon.service /etc/systemd/system/nix-daemon.socket /etc/tmpfiles.d/nix-daemon.conf
systemctl daemon-reload
cp -a /etc/bash.bashrc.backup-before-nix /etc/bash.bashrc
mv /etc/bash.bashrc.backup-before-nix "$base/bash.bashrc.before-nix"
rm /etc/profile.d/nix.sh /etc/nix/nix.conf
rmdir /etc/nix
if test -d /var/cache/homecloud-nix-lab/{run}; then
    test ! -L /var/cache/homecloud-nix-lab/{run}
    test "$(stat -c %U /var/cache/homecloud-nix-lab/{run}/client-a)" = hcnix-a
    test "$(stat -c %U /var/cache/homecloud-nix-lab/{run}/client-b)" = hcnix-b
    rm -r /var/cache/homecloud-nix-lab/{run}
    rmdir /var/cache/homecloud-nix-lab
fi
for user in hcnix-a hcnix-b; do
    test "$(getent passwd "$user" | cut -d: -f6)" = "/var/lib/homecloud/nix-lab-users/$user"
    userdel --remove "$user"
done
rmdir /var/lib/homecloud/nix-lab-users
for number in $(seq 1 32); do userdel "nixbld$number"; done
groupdel nixbld
groupdel hcnix-no-builders
if test "$(readlink /root/.nix-profile)" = /nix/var/nix/profiles/per-user/root/profile; then rm /root/.nix-profile; fi
if test "$(readlink /root/.nix-defexpr/channels)" = /nix/var/nix/profiles/per-user/root/channels; then rm /root/.nix-defexpr/channels; rmdir /root/.nix-defexpr; fi
# The dataset is tagged with this manifest's UUID and has no children/snapshots.
zfs destroy {dataset}
if test -d /nix; then rmdir /nix; fi
for stage in /var/tmp/hcnix-install.*; do
    if test -f "$stage/.homecloud-nix-lab" && test "$(cat "$stage/.homecloud-nix-lab")" = {run}; then
        cp "$stage/install.log" "$base/installer.log"
        rm -r "$stage"
    fi
done
rm "$base/ssh_host_ed25519_key" "$base/ssh_host_ed25519_key.pub" "$base/builder_ed25519" "$base/builder_ed25519.pub"
rm "$base/active"
printf 'Owned Nix services, shares, users, firewall exceptions, and dataset removed\n'
"#));
        println!("{}", self.root_step(&script).await?);
        Ok(())
    }
}
