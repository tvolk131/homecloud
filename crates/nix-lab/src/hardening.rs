use super::*;

impl Runner {
    pub(super) async fn harden(&self, phase: &str) -> Result<()> {
        let s = self.state.as_ref().context("Prepare first")?;
        let run = s.run.simple().to_string();
        let base = format!("/var/lib/homecloud/nix-lab/{run}");
        let a = format!(
            "hc-{}",
            s.fixtures
                .iter()
                .find(|f| f.role == "client-a")
                .unwrap()
                .id
                .context("Fixture ID")?
                .simple()
        );
        let b = format!(
            "hc-{}",
            s.fixtures
                .iter()
                .find(|f| f.role == "client-b")
                .unwrap()
                .id
                .context("Fixture ID")?
                .simple()
        );
        let state_a = format!("/nix/lab-roots/{run}/client-a");
        let state_b = format!("/nix/lab-roots/{run}/client-b");
        let guest = |vm: &str, code: &str| {
            format!(
                "incus exec {} -- runuser -u dev -- sh -se -c {}",
                quote(vm),
                quote(format!(
                    ". /etc/profile.d/hcnix.sh\nexport PATH=/usr/local/bin:/usr/bin:/bin\n{code}"
                ))
            )
        };
        let body = match phase {
            "root-isolation" => {
                let code = format!(
                    r#"
if nix build --file /home/dev/hcnix-proof/package.nix --out-link {state_b}/forbidden-client-a > "$XDG_STATE_HOME/cross-root.log" 2>&1; then
    echo 'Client A unexpectedly created a root in client B state' >&2; exit 1
fi
cat "$XDG_STATE_HOME/cross-root.log"
grep -F 'Permission denied' "$XDG_STATE_HOME/cross-root.log"
"#
                );
                format!(
                    "{}\ntest ! -e {state_b}/forbidden-client-a\ntest ! -L {state_b}/forbidden-client-a\nprintf '%s\\n' cross-client-root-denied\n",
                    guest(&a, &code)
                )
            }

            "concurrency" => {
                let mut script = format!(
                    r#"
base={base}
sed 's/hcnix-shared-/hcnix-race-/; s/mkdir -p/sleep 4; mkdir -p/' "$base/proof.nix" > "$base/race.nix"
for state in {state_a} {state_b}; do cp "$base/race.nix" "$state/race.nix"; done
"#
                );
                for (vm, label) in [(&a, "a"), (&b, "b")] {
                    script.push_str(&format!(
                        "{} > \"$base/race-{label}.log\" 2>&1 &\npid_{label}=$!\n",
                        guest(vm, "nix build --file \"$XDG_STATE_HOME/race.nix\" --out-link \"$XDG_STATE_HOME/race-result\" --print-out-paths > \"$XDG_STATE_HOME/race-output\"\noutput=$(cat \"$XDG_STATE_HOME/race-output\"); \"$output/bin/hcnix-proof\"")
                    ));
                }
                script.push_str(&format!(
                    r#"
wait "$pid_a"
wait "$pid_b"
test "$(cat {state_a}/race-output)" = "$(cat {state_b}/race-output)"
output=$(cat {state_a}/race-output)
test "$(cat "$output/builder-marker")" = {run}
/nix/var/nix/profiles/default/bin/nix-store --verify-path "$output"
printf 'concurrent-build-output=%s\n' "$output"
"#
                ));
                script
            }
            "runtime-gc"
            | "runtime-lease"
            | "runtime-client-gc"
            | "runtime-gc-lock"
            | "runtime-client-gc-lock" => {
                let expr = r#"let
  pkgs = import (builtins.getFlake "github:NixOS/nixpkgs/@REV@").outPath { system = "x86_64-linux"; };
in pkgs.runCommandNoCC "hcnix-@PHASE@-@RUN@" { meta.mainProgram = "runtime-proof"; } ''
  mkdir -p "$out/bin" "$out/share"
  echo 'late-file-read-ok' > "$out/share/token"
  cat > "$out/bin/runtime-proof" <<SCRIPT
#!${pkgs.bash}/bin/bash
set -eu
/usr/bin/touch "\$1"
while ! test -f "\$2"; do /usr/bin/sleep 1; done
/usr/bin/cat "$out/share/token" > "\$3"
SCRIPT
  chmod +x "$out/bin/runtime-proof"
''
"#;
                let mut script = format!(
                    r#"
base={base}
state={state_a}
rev=$(jq -r .locked.rev "$base/nixpkgs-metadata.json")
cat > "$state/{phase}.nix.in" <<'EXPR'
{expr}EXPR
sed "s/@REV@/$rev/g; s/@PHASE@/{phase}/g; s/@RUN@/{run}/g" "$state/{phase}.nix.in" > "$state/{phase}.nix"
rm -f "$state/{phase}-ready" "$state/{phase}-release" "$state/{phase}-observed"
"#
                );
                if phase == "runtime-lease" {
                    script.push_str(&format!("{}\n", guest(&a, &format!("nix build --file \"$XDG_STATE_HOME/{phase}.nix\" --out-link \"$XDG_STATE_HOME/{phase}-lease\" --print-out-paths > \"$XDG_STATE_HOME/{phase}-output\""))));
                } else {
                    script.push_str(&format!("{}\n",guest(&a, &format!("nix eval --raw --file \"$XDG_STATE_HOME/{phase}.nix\" outPath > \"$XDG_STATE_HOME/{phase}-output\""))));
                }
                script.push_str(&format!(
                    "{} > \"$base/{phase}-guest.log\" 2>&1 &\nchild=$!\n",
                    guest(&a, &format!("timeout 300 nix run --file \"$XDG_STATE_HOME/{phase}.nix\" '' -- \"$XDG_STATE_HOME/{phase}-ready\" \"$XDG_STATE_HOME/{phase}-release\" \"$XDG_STATE_HOME/{phase}-observed\""))
                ));
                script.push_str(&format!(r#"
trap 'touch "$state/{phase}-release"' EXIT
attempt=0
until test -f "$state/{phase}-ready"; do
    kill -0 "$child" || {{ cat "$base/{phase}-guest.log"; exit 1; }}
    attempt=$((attempt+1)); test "$attempt" -lt 180; sleep 1
done
output=$(cat "$state/{phase}-output")
/nix/var/nix/profiles/default/bin/nix-store --query --roots "$output" > "$base/{phase}-roots-before.txt"
"#));
                let delete = if matches!(phase, "runtime-client-gc" | "runtime-client-gc-lock") {
                    format!(
                        "incus exec {b} -- runuser -u dev -- env NIX_REMOTE=mounted-ssh-ng://hc-nix-store /usr/local/bin/nix-store --delete \"$output\""
                    )
                } else {
                    "/nix/var/nix/profiles/default/bin/nix-store --delete \"$output\"".to_owned()
                };
                if matches!(phase, "runtime-gc-lock" | "runtime-client-gc-lock") {
                    script.push_str(&format!(r#"
rm -f "$base/gc-lease-ready" "$base/gc-lease-release"
test -x "$base/gc-lease"
"$base/gc-lease" /nix/var/nix/gc.lock "$base/gc-lease-ready" "$base/gc-lease-release" > "$base/gc-lease.log" 2>&1 &
lease_pid=$!
trap 'touch "$state/{phase}-release" "$base/gc-lease-release"' EXIT
attempt=0
until test -f "$base/gc-lease-ready"; do kill -0 "$lease_pid"; attempt=$((attempt+1)); test "$attempt" -lt 30; sleep 1; done
# Publication must continue while the shared GC lock is held.
sed 's/hcnix-shared-/hcnix-{phase}-publication-/' "$base/proof.nix" > "$base/lease-publication.nix"
/nix/var/nix/profiles/default/bin/nix build --file "$base/lease-publication.nix" --no-link --print-out-paths > "$base/lease-publication-output"
{delete} > "$base/{phase}-delete.log" 2>&1 &
collector=$!
sleep 2
kill -0 "$collector"
test -e "$output"
retained=true
touch "$state/{phase}-release"
if wait "$child"; then run_ok=true; else run_ok=false; fi
test "$run_ok" = true
touch "$base/gc-lease-release"
wait "$lease_pid"
wait "$collector"
test ! -e "$output"
delete_rc=0
"#));
                } else {
                    script.push_str(&format!(
                        r#"
if {delete} > "$base/{phase}-delete.log" 2>&1; then delete_rc=0; else delete_rc=$?; fi
if test -e "$output"; then retained=true; else retained=false; fi
touch "$state/{phase}-release"
if wait "$child"; then run_ok=true; else run_ok=false; fi
"#
                    ));
                }
                script.push_str(&format!(r#"
jq -n --arg phase {phase} --arg path "$output" --argjson retained "$retained" --argjson runOk "$run_ok" --argjson deleteExit "$delete_rc" '{{phase:$phase,path:$path,retainedDuringExecution:$retained,lateReadSucceeded:$runOk,deleteExit:$deleteExit}}' | tee "$base/{phase}.json"
trap - EXIT
"#));
                if phase == "runtime-lease" {
                    script.push_str(&format!(
                        r#"
test "$retained" = true
test "$run_ok" = true
rm "$state/{phase}-lease"
/nix/var/nix/profiles/default/bin/nix-store --delete "$output"
test ! -e "$output"
"#
                    ));
                }
                script
            }
            "full-gc" => {
                let mut script = format!("base={base}\n");
                for (vm, state, label) in [(&a, &state_a, "a"), (&b, &state_b, "b")] {
                    script.push_str(&format!("rm -f {state}/gc-ready {state}/gc-release\n"));
                    let code = r#"cd /home/dev/fedimint
nix develop --command bash -euc '
  touch "$XDG_STATE_HOME/gc-ready"
  until test -f "$XDG_STATE_HOME/gc-release"; do sleep 1; done
  rustc --version
  cargo --version
  openssl version
'
"#;
                    script.push_str(&format!(
                        "{} > \"$base/full-gc-{label}.log\" 2>&1 &\npid_{label}=$!\n",
                        guest(vm, &format!("timeout 600 sh -se -c {}", quote(code)))
                    ));
                }
                script.push_str(&format!(
                    r#"
trap 'touch {state_a}/gc-release {state_b}/gc-release' EXIT
attempt=0
until test -f {state_a}/gc-ready && test -f {state_b}/gc-ready; do
    kill -0 "$pid_a"; kill -0 "$pid_b"
    attempt=$((attempt+1)); test "$attempt" -lt 300; sleep 1
done
/nix/var/nix/profiles/default/bin/nix-store --gc > "$base/full-gc-collector.log" 2>&1
touch {state_a}/gc-release {state_b}/gc-release
wait "$pid_a"
wait "$pid_b"
cat "$base/full-gc-a.log" "$base/full-gc-b.log"
tail -5 "$base/full-gc-collector.log"
trap - EXIT
"#
                ));
                script
            }
            _ => unreachable!(),
        };
        println!("{}", self.root_step(&body).await?);
        Ok(())
    }
}
