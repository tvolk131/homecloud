use super::*;

impl Runner {
    pub async fn fedimint(&self, phase: &str, source: Option<&Path>) -> Result<()> {
        self.verify().await?;
        let s = self.state.as_ref().context("Prepare fixtures first")?;
        let run = s.run.simple().to_string();
        let base = format!("/var/lib/homecloud/nix-lab/{run}");
        let vm = |role: &str| -> Result<String> {
            Ok(format!(
                "hc-{}",
                s.fixtures
                    .iter()
                    .find(|f| f.role == role)
                    .context("Fixture role")?
                    .id
                    .context("Fixture ID")?
                    .simple()
            ))
        };
        let a = vm("client-a")?;
        let b = vm("client-b")?;
        let builder = vm("builder")?;
        let guest = |vm: &str, code: &str| {
            format!(
                "incus exec {} -- runuser -u dev -- sh -se -c {}\n",
                quote(vm),
                quote(format!(
                    ". /etc/profile.d/hcnix.sh\nexport PATH=/usr/local/bin:/usr/bin:/bin\ncd /home/dev/fedimint\n{code}"
                ))
            )
        };
        if phase == "cache" {
            println!("{}", self.root_step(&self.fedimint_cache_script()?).await?);
            return Ok(());
        }
        if phase == "source" {
            let source = source.context("source requires --source /path/to/clean/fedimint")?;
            let mut status = Command::new("git");
            status
                .arg("-C")
                .arg(source)
                .args(["status", "--porcelain", "--untracked-files=no"]);
            ensure!(
                execute(status, None, 30).await?.trim().is_empty(),
                "Fedimint has tracked changes; choose an explicit clean snapshot"
            );
            let mut rev = Command::new("git");
            rev.arg("-C").arg(source).args(["rev-parse", "HEAD"]);
            let rev = execute(rev, None, 30).await?.trim().to_owned();
            ensure!(
                rev.len() == 40 && rev.bytes().all(|c| c.is_ascii_hexdigit()),
                "Unexpected source revision"
            );
            let archive = self.args.state.with_extension("fedimint.tar.gz");
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(source)
                .args(["archive", "--format=tar.gz", "--output"])
                .arg(&archive)
                .arg("HEAD");
            execute(command, None, 120).await?;
            self.remote(
                &format!(
                    "{}test -f {base}/active\ntest ! -e {base}/fedimint-source\ninstall -d -m 0700 {base}/fedimint-source\nprintf '%s\\n' {} > {base}/fedimint-original-revision\n",
                    self.fence()?, quote(&rev)
                ),
                60,
            ).await?;
            let mut command = Command::new("ssh");
            command
                .arg("-F")
                .arg(&self.args.ssh_config)
                .args(["-o", "BatchMode=yes", "hclab-host"])
                .arg(format!(
                    "sudo -n tar -xzf - -C {}",
                    quote(format!("{base}/fedimint-source"))
                ));
            execute(command, Some(&fs::read(&archive)?), 180).await?;
            let mut script = format!("base={base}\n");
            // This cache key is explicitly published by the pinned project's flake.
            // Untrusted guests cannot add arbitrary signing authorities to the host.
            let cache = "extra-substituters = https://fedimint.cachix.org\nextra-trusted-public-keys = fedimint.cachix.org-1:FpJJjy1iPVlvyv4OMiN5y9+/arFLPcnZhZVVCHCDYTs=\n";
            script.push_str(&format!(
                "printf '%s' {} >> /etc/nix/nix.conf\nsystemctl restart nix-daemon\nprintf '%s' {} | incus exec {builder} -- sh -c 'cat >> /etc/nix/nix.conf; systemctl restart nix-daemon'\n",
                quote(cache), quote(cache)
            ));
            for (role, vm, user) in [("client-a", &a, "hcnix-a"), ("client-b", &b, "hcnix-b")] {
                let state = format!("/nix/lab-roots/{run}/{role}");
                script.push_str(&format!(r#"
state={state}
uid=$(incus exec {vm} -- id -u dev)
gid=$(incus exec {vm} -- id -g dev)
install -d "$state/config" "$state/workspace/fedimint"
cp -r "$base/fedimint-source/." "$state/workspace/fedimint/"
chown -R "$uid:$gid" "$state/config" "$state/workspace"
rev=$(jq -r .locked.rev "$base/nixpkgs-metadata.json")
acl_store=$(/nix/var/nix/profiles/default/bin/nix build --no-link --print-out-paths --expr "let pkgs = import (builtins.getFlake \"github:NixOS/nixpkgs/$rev\").outPath {{ system = \"x86_64-linux\"; }}; in pkgs.lib.getBin pkgs.acl")
"$acl_store/bin/setfacl" -R -m u:{user}:rwX "$state/config" "$state/workspace"
find "$state/config" "$state/workspace" -type d -exec "$acl_store/bin/setfacl" -m d:u:{user}:rwx,d:u:"$uid":rwx {{}} +
incus exec {vm} -- sh -se -c 'printf "%s\n" "accept-flake-config = true" >> /etc/nix/nix.conf; ln -s {state}/workspace/fedimint /home/dev/fedimint'
"#));
                script.push_str(&guest(vm, r#"
git init -q
git config user.name 'Homecloud Nix lab'
git config user.email 'nix-lab@localhost'
git add -f .
GIT_AUTHOR_DATE=2026-10-31T00:00:00Z GIT_COMMITTER_DATE=2026-10-31T00:00:00Z git commit -qm 'Pinned Fedimint workload snapshot'
printf 'workspace=%s\n' "$(pwd -P)"
"#));
            }
            script.push_str(&self.fedimint_cache_script()?);
            println!("{}", self.root_step(&script).await?);
            println!(
                "Pinned Fedimint source revision {rev}; untracked files and Git credentials excluded"
            );
            return Ok(());
        }
        let body = match phase {
            "metadata" => guest(
                &a,
                r#"
nix flake metadata --no-write-lock-file --json . > "$XDG_STATE_HOME/fedimint-metadata.json"
nix eval --raw .#devShells.x86_64-linux.default.drvPath > "$XDG_STATE_HOME/fedimint-shell-drv"
cat "$XDG_STATE_HOME/fedimint-shell-drv"
"#,
            ),
            "develop" | "reuse" => {
                let vm = if phase == "develop" { &a } else { &b };
                guest(
                    vm,
                    r#"
start=$(date +%s)
# Deliberately exercise the default project-local result link and default dev profile.
nix build .#devShells.x86_64-linux.default --print-out-paths > "$XDG_STATE_HOME/fedimint-shell-output"
readlink -f result
nix develop --command bash -euc '
  rustc --version
  cargo --version
  export CARGO_TARGET_DIR=/home/dev/fedimint-target
  cargo test --locked -p fedimint-hkdf -p fedimint-aead -p fedimint-util-error
  rustc_path=$(readlink -f "$(command -v rustc)")
  printf "%s\n" "$rustc_path" > "$XDG_STATE_HOME/fedimint-rustc"
  stat -c "rustc-inode=%i bytes=%s path=%n" "$rustc_path"
'
printf 'elapsed-seconds=%s\n' "$(($(date +%s)-start))"
"#,
                )
            }
            "measure" => format!(
                r#"
base={base}
a=/nix/lab-roots/{run}/client-a
b=/nix/lab-roots/{run}/client-b
test "$(cat "$a/fedimint-shell-output")" = "$(cat "$b/fedimint-shell-output")"
test "$(cat "$a/fedimint-rustc")" = "$(cat "$b/fedimint-rustc")"
rustc=$(cat "$a/fedimint-rustc")
stat -c 'host-rustc-inode=%i bytes=%s path=%n' "$rustc"
for vm in {a} {b}; do
    incus exec "$vm" -- stat -c 'guest-rustc-inode=%i bytes=%s path=%n' "$rustc"
    incus exec "$vm" -- findmnt -n -o SOURCE,FSTYPE,OPTIONS /nix/store
    incus exec "$vm" -- du -sx -B1 /home/dev/fedimint-target
    incus exec "$vm" -- df -B1 / /nix/store
done
/nix/var/nix/profiles/default/bin/nix path-info --json --recursive "$(cat "$a/fedimint-shell-output")" > "$base/fedimint-closure.json"
jq '{{paths:length,narBytes:([.[].narSize]|add)}}' "$base/fedimint-closure.json"
du -sx -B1 /nix/store
zfs list -Hp -o name,used,referenced,logicalused hclab/nix-lab-{run}
"#
            ),
            _ => anyhow::bail!(
                "Unknown Fedimint phase; choose source, cache, metadata, develop, reuse, measure"
            ),
        };
        let allocation = |label: &str| {
            format!(
                r#"
allocated=$(du -sx -B1 /nix/store | cut -f1)
paths=$(find /nix/store -mindepth 1 -maxdepth 1 | wc -l)
dataset=$(zfs list -Hp -o used hclab/nix-lab-{run})
jq -n --argjson storeAllocated "$allocated" --argjson entries "$paths" --argjson datasetUsed "$dataset" '{{storeAllocatedBytes:$storeAllocated,topLevelEntries:$entries,datasetUsedBytes:$datasetUsed}}' > "$base/fedimint-{phase}-{label}.json"
"#
            )
        };
        let body = if matches!(phase, "develop" | "reuse") {
            format!("{}\n{body}\n{}", allocation("before"), allocation("after"))
        } else {
            body
        };
        let script = self.fence()?
            + &logged_script(
                &format!("base={base}\ntest -f \"$base/active\"\n{body}"),
                &format!("{base}/fedimint-{phase}.log"),
            );
        println!("{}", self.remote(&script, 7200).await?);
        self.verify().await?;
        Ok(())
    }
    fn fedimint_cache_script(&self) -> Result<String> {
        let s = self.state.as_ref().context("Prepare first")?;
        let run = s.run.simple().to_string();
        let mut script = String::new();
        for (role, user) in [("client-a", "hcnix-a"), ("client-b", "hcnix-b")] {
            let vm = format!(
                "hc-{}",
                s.fixtures
                    .iter()
                    .find(|f| f.role == role)
                    .unwrap()
                    .id
                    .context("Fixture ID")?
                    .simple()
            );
            let state = format!("/nix/lab-roots/{run}/{role}");
            let cache = format!("/var/cache/homecloud-nix-lab/{run}/{role}");
            script.push_str(&format!(r#"
install -d -m 0755 /var/cache/homecloud-nix-lab /var/cache/homecloud-nix-lab/{run}
install -d -o {user} -g {user} -m 0700 {cache} {cache}/nix
incus exec {vm} -- install -d -o dev -g dev -m 0700 {cache} {cache}/nix
incus exec {vm} -- findmnt -n -T {cache} -o SOURCE,FSTYPE,OPTIONS
printf '%s\n' 'export NIX_REMOTE=mounted-ssh-ng://hc-nix-store' 'export XDG_STATE_HOME={state}' 'export XDG_CACHE_HOME={cache}' 'export XDG_CONFIG_HOME={state}/config' | incus exec {vm} -- sh -c 'cat > /etc/profile.d/hcnix.sh'
"#));
        }
        Ok(script)
    }
}
