# Client device credentials

A device key authorizes one enrolled device. It is separate from the user's
identity, VM SSH keys, provider credentials and the host's Iroh endpoint key.

On macOS, Homecloud stores the device key in the login Keychain as a native
generic-password item under service `dev.homecloud.device-credentials`. Its
account is derived from the pinned server ID and public device ID. The GUI and
CLI share that item. The JSON profile contains public endpoint metadata and a
versioned credential reference; copying that file alone does not copy access.

The first successful load migrates an existing plaintext Mac profile. Migration
serializes concurrent writers, keeps the exact existing key and server pin,
verifies the Keychain write by reading it back, and only then atomically replaces
the old profile. There is no plaintext fallback. Denied/locked Keychain access,
a missing/corrupt key, a mismatched reference or an unknown profile version
produces an error instead of silently creating another device. An unsuccessful
migration keeps the previous profile usable for a retry.

Unlock the login Keychain and permit access for the intended Homecloud binary if
macOS asks. Reopen Homecloud after unlocking Keychain if startup was blocked. Development builds use ad-hoc signing, so macOS may ask again after a
build or when a different executable first accesses the item. Stable release
signing/notarization is still follow-up packaging work. Do not grant access to
all applications or disable Keychain protection to suppress a prompt.

Linux clients currently retain private mode-0600 file credentials. A macOS
credential-reference profile cannot be used as a Linux device; enroll that device
separately. Host endpoint-key storage and guest AI-provider credentials are
unchanged by this client migration.

For device loss, revoke the device and enroll a replacement. Lost-admin recovery
still requires local privileged host authority; see [operations](operations.md).
Deleting a profile does not revoke its device, and deleting a Keychain item does
not remove the server's authorization record. Keychain/profile cleanup must not
be confused with revocation.

Automated migration and transport tests use injected credential stores and never
write to the real login Keychain. The explicitly ignored native smoke test writes
and deletes one disposable item. Run it only on an unlocked interactive Mac:

```sh
cargo test --locked -p homecloud-client native_keychain_roundtrip_and_delete \
  -- --ignored
```
