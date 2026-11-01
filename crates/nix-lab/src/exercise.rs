use super::*;
impl Runner {
    pub async fn exercise(&self, phase: &str) -> Result<()> {
        if matches!(
            phase,
            "concurrency"
                | "runtime-gc"
                | "runtime-client-gc"
                | "runtime-gc-lock"
                | "runtime-client-gc-lock"
                | "runtime-lease"
                | "full-gc"
                | "root-isolation"
        ) {
            return self.harden(phase).await;
        }
        let s = self.state.as_ref().context("Prepare first")?;
        let run = s.run.simple().to_string();
        let base = format!("/var/lib/homecloud/nix-lab/{run}");
        let name = |role: &str| -> String {
            format!(
                "hc-{}",
                s.fixtures
                    .iter()
                    .find(|f| f.role == role)
                    .unwrap()
                    .id
                    .unwrap()
                    .simple()
            )
        };
        let a = name("client-a");
        let b = name("client-b");
        let nix = "/nix/var/nix/profiles/default/bin/nix";
        let guest = |vm: &str, code: &str| -> String {
            format!(
                "incus exec {} -- runuser -u dev -- env NIX_REMOTE=mounted-ssh-ng://hc-nix-store PATH=/usr/local/bin:/usr/bin:/bin sh -se -c {}\n",
                quote(vm),
                quote(code)
            )
        };
        let script = match phase {
            "connect" => {
                let mut out = String::new();
                for vm in [&a, &b] {
                    out.push_str(&guest(vm,"nix --version; nix store info --json; nix config show | grep -E '^(store|experimental-features) ='"));
                }
                out
            }
            "source" => format!(
                r#"
base={base}
if ! test -f "$base/nixpkgs-metadata.json"; then
    {nix} flake metadata github:NixOS/nixpkgs/nixos-26.05 --json > "$base/nixpkgs-metadata.json.tmp"
    mv "$base/nixpkgs-metadata.json.tmp" "$base/nixpkgs-metadata.json"
fi
jq '{{revision:.locked.rev,narHash:.locked.narHash,path}}' "$base/nixpkgs-metadata.json"
"#
            ),
            "build" => {
                let expr = format!(
                    r#"let
  pkgs = import (builtins.getFlake "github:NixOS/nixpkgs/@REV@").outPath {{ system = "x86_64-linux"; }};
in pkgs.runCommandNoCC "hcnix-shared-{run}" {{ meta.mainProgram = "hcnix-proof"; }} ''
  mkdir -p "$out/bin"
  cat /hc-builder-marker > "$out/builder-marker"
  cat > "$out/bin/hcnix-proof" <<'SCRIPT'
#!${{pkgs.bash}}/bin/bash
printf '%s\n' 'shared Nix build {run}'
SCRIPT
  chmod +x "$out/bin/hcnix-proof"
''
"#
                );
                let mut out = format!(
                    r#"
base={base}
rev=$(jq -r .locked.rev "$base/nixpkgs-metadata.json")
cat > "$base/proof.nix.in" <<'EXPR'
{expr}EXPR
sed "s/@REV@/$rev/g" "$base/proof.nix.in" > "$base/proof.nix"
for vm in {a} {b}; do
    incus exec "$vm" -- install -d -o dev -g dev /home/dev/hcnix-proof
    cat "$base/proof.nix" | incus exec "$vm" -- sh -c 'cat > /home/dev/hcnix-proof/package.nix; chown dev:dev /home/dev/hcnix-proof/package.nix'
done
"#
                );
                out.push_str(&guest(&a,"cd /home/dev/hcnix-proof; nix build --file package.nix --no-link --print-out-paths > /home/dev/hcnix-proof/output"));
                out.push_str(&format!(r#"
output=$(incus exec {a} -- cat /home/dev/hcnix-proof/output)
test "$(cat "$output/builder-marker")" = {run}
printf '%s\n' "$output" > "$base/output"
# Preserve the result explicitly until guest-root integration is tested.
/nix/var/nix/profiles/default/bin/nix-store --add-root "$base/proof-root" --realise "$output" >/dev/null
"#));
                out.push_str(&guest(&b,"cd /home/dev/hcnix-proof; nix build --file package.nix --no-link --print-out-paths > output; output=$(cat output); \"$output/bin/hcnix-proof\"; stat -c '%i %s %n' \"$output/bin/hcnix-proof\"; findmnt -n -o SOURCE,FSTYPE,OPTIONS /nix/store"));
                out.push_str(&format!(
                    r#"
test "$(incus exec {b} -- cat /home/dev/hcnix-proof/output)" = "$(cat "$base/output")"
stat -c '%i %s %n' "$output/bin/hcnix-proof"
zfs list -Hp -o name,used,referenced hclab/nix-lab-{run}
"#
                ));
                out
            }
            "guest-private-root" => guest(
                &a,
                r#"
cd /home/dev/hcnix-proof
if nix build --file package.nix > private-root.log 2>&1; then
    echo 'Guest-private result path unexpectedly worked' >&2; exit 1
fi
cat private-root.log
grep -F 'Permission denied' private-root.log
"#,
            ),
            "live" => {
                let mut out = format!(
                    r#"
base={base}
sed 's/hcnix-shared-/hcnix-live-/' "$base/proof.nix" > "$base/live.nix"
for vm in {a} {b}; do
    cat "$base/live.nix" | incus exec "$vm" -- sh -c 'cat > /home/dev/hcnix-proof/live.nix; chown dev:dev /home/dev/hcnix-proof/live.nix'
done
"#
                );
                out.push_str(&guest(&b, "cd /home/dev/hcnix-proof; nix eval --raw --file live.nix outPath > live-output; output=$(cat live-output); test ! -e \"$output\"; findmnt -n -o ID /nix/store > mount-before; printf 'negative-lookup=%s\\n' \"$output\""));
                out.push_str(&guest(&a, "cd /home/dev/hcnix-proof; nix build --file live.nix --no-link --print-out-paths"));
                out.push_str(&guest(&b, "cd /home/dev/hcnix-proof; output=$(cat live-output); test -e \"$output\"; \"$output/bin/hcnix-proof\"; test \"$(cat mount-before)\" = \"$(findmnt -n -o ID /nix/store)\"; printf 'unchanged-mount-id=%s\\n' \"$(cat mount-before)\"; stat -c '%i %n' \"$output/bin/hcnix-proof\""));
                out.push_str(&format!(
                    r#"
output=$(incus exec {b} -- cat /home/dev/hcnix-proof/live-output)
printf '%s\n' "$output" > {base}/live-output
stat -c '%i %n' "$output/bin/hcnix-proof"
for vm in {a} {b}; do
    if incus exec "$vm" -- touch /nix/store/hcnix-root-write-probe-{run}; then
        echo 'Root wrote to the shared store' >&2; exit 1
    fi
done
"#
                ));
                out
            }
            "rooted-commands" => {
                let state = format!("/nix/lab-roots/{run}/client-a");
                let command = format!(
                    r#"
export XDG_STATE_HOME={state}
cd /home/dev/hcnix-proof
nix build --file package.nix --out-link "$XDG_STATE_HOME/result"
nix develop --file package.nix --profile "$XDG_STATE_HOME/dev-profile" --command sh -c 'printf "develop-ok\n"'
nix profile add --file package.nix
nix profile list
"#
                );
                guest(&a, &command)
            }
            "gc" => {
                let state = format!("/nix/lab-roots/{run}/client-a");
                format!(
                    r#"
base={base}
output=$(cat "$base/output")
rm "$base/proof-root"
# The host must now find the roots created by the client's shared state paths.
{nix}-store --gc --print-roots | grep -F {state}
{nix}-store --gc --print-live | grep -Fx "$output"
if {nix}-store --delete "$output" > "$base/gc-protected.log" 2>&1; then
    echo 'Collector deleted rooted output' >&2; exit 1
fi
cat "$base/gc-protected.log"
# A separate unrooted object is safe to reclaim, and disappears from both VMs.
live=$(cat "$base/live-output")
{nix}-store --delete "$live"
for vm in {a} {b}; do
    incus exec "$vm" -- test ! -e "$live"
    incus exec "$vm" -- test -e "$output/bin/hcnix-proof"
done
printf 'shared-roots-protect-output; unrooted-output-reclaimed\n'
"#
                )
            }
            "isolation" => {
                let mut out = format!(
                    r#"
base={base}
sed 's/hcnix-shared-/hcnix-local-denied-/; s/meta.mainProgram = "hcnix-proof";/meta.mainProgram = "hcnix-proof"; preferLocalBuild = true;/' "$base/proof.nix" > "$base/local-denied.nix"
cat "$base/local-denied.nix" | incus exec {a} -- sh -c 'cat > /home/dev/hcnix-proof/local-denied.nix; chown dev:dev /home/dev/hcnix-proof/local-denied.nix'
"#
                );
                out.push_str(&guest(&a, &format!(r#"
cd /home/dev/hcnix-proof
output=$(nix build --file local-denied.nix --no-link --print-out-paths --max-jobs 1 --option build-users-group nixbld --option sandbox false)
test "$(cat "$output/builder-marker")" = {run}
printf 'guest-override-request-still-built-in-worker\n'
"#)));
                out.push_str(&format!(r#"
base={base}
client_home=/var/lib/homecloud/nix-lab-users/hcnix-a
sed 's/hcnix-local-denied-/hcnix-host-denied-/' "$base/local-denied.nix" > "$client_home/host-denied.nix"
chown hcnix-a:hcnix-a "$client_home/host-denied.nix"
if runuser -u hcnix-a -- env NIX_REMOTE=daemon {nix} build --file "$client_home/host-denied.nix" --no-link --max-jobs 1 --option builders '' --option build-users-group nixbld --option sandbox false > "$base/isolation.log" 2>&1; then
    cat "$base/isolation.log"; echo 'Direct daemon client unexpectedly enabled a local build' >&2; exit 1
fi
cat "$base/isolation.log"
grep -F 'has no members' "$base/isolation.log"
grep -F "ignoring the client-specified setting 'build-users-group'" "$base/isolation.log"
grep -F "ignoring the client-specified setting 'sandbox'" "$base/isolation.log"
"#));
                out
            }
            "recovery" => {
                let mut out = format!(
                    r#"
output=$(cat {base}/output)
systemctl stop hcnix-lab-sshd
trap 'systemctl start hcnix-lab-sshd' EXIT
incus exec {b} -- runuser -u dev -- "$output/bin/hcnix-proof"
if incus exec {b} -- runuser -u dev -- timeout 8 env NIX_REMOTE=mounted-ssh-ng://hc-nix-store /usr/local/bin/nix store info --json; then
    echo 'Endpoint unexpectedly available' >&2; exit 1
fi
systemctl start hcnix-lab-sshd
trap - EXIT
old_boot=$(incus exec {b} -- cat /proc/sys/kernel/random/boot_id)
# Reboot may close the agent websocket before systemctl returns.
incus exec {b} -- systemctl reboot || true
attempt=0
until new_boot=$(incus exec {b} -- cat /proc/sys/kernel/random/boot_id) && test "$new_boot" != "$old_boot" && incus exec {b} -- test -e "$output/bin/hcnix-proof" && incus exec {b} -- runuser -u dev -- timeout 5 env NIX_REMOTE=mounted-ssh-ng://hc-nix-store /usr/local/bin/nix store info --json > /dev/null; do
    attempt=$((attempt+1)); test "$attempt" -lt 60; sleep 1
done
"#
                );
                out.push_str(&guest(&b, "nix store info --json; uname -r; output=$(cat /home/dev/hcnix-proof/output); \"$output/bin/hcnix-proof\"; findmnt -n -o SOURCE,FSTYPE,OPTIONS /nix/store"));
                out.push_str(&format!("if incus exec {b} -- touch /nix/store/hcnix-reboot-write-probe-{run}; then exit 1; fi\n"));
                out
            }
            _ => anyhow::bail!(
                "Choose connect, source, build, guest-private-root, live, rooted-commands, gc, isolation, recovery"
            ),
        };
        let result = self.root_step(&script).await;
        let path = self
            .args
            .state
            .with_file_name(format!("evidence-{phase}.txt"));
        match result {
            Ok(output) => {
                private_write(&path, output.as_bytes())?;
                println!("{output}");
                Ok(())
            }
            Err(error) => {
                private_write(&path, format!("FAILED: {error:#}\n").as_bytes())?;
                Err(error)
            }
        }
    }
}
